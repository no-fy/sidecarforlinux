//! A thin, safe-ish layer over the libva calls the native encoder uses.
//! Every object holds the raw display handle; `Display` must outlive them
//! (the encoder declares it last, so it is dropped last).
//!
//! **Built but untested on real hardware**: there is no GPU in the VM or on
//! CI runners. Error messages name the failing call so a log from your
//! machine says where it broke.

use super::va_sys as sys;
use anyhow::{anyhow, bail, Context as _, Result};
use std::ffi::{c_void, CStr};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

fn check(call: &str, status: sys::VAStatus) -> Result<()> {
    if status as u32 == sys::VA_STATUS_SUCCESS {
        return Ok(());
    }
    // SAFETY: vaErrorStr returns a static string for any status.
    let msg = unsafe { CStr::from_ptr(sys::vaErrorStr(status)) }.to_string_lossy();
    bail!("{call} failed: {msg} (VA status {status:#x})")
}

unsafe extern "C" fn log_info(_: *mut c_void, msg: *const std::os::raw::c_char) {
    let m = CStr::from_ptr(msg).to_string_lossy();
    tracing::debug!("libva: {}", m.trim_end());
}

unsafe extern "C" fn log_error(_: *mut c_void, msg: *const std::os::raw::c_char) {
    let m = CStr::from_ptr(msg).to_string_lossy();
    tracing::warn!("libva: {}", m.trim_end());
}

pub struct Display {
    pub dpy: sys::VADisplay,
    pub path: PathBuf,
    pub vendor: String,
    _fd: OwnedFd,
}

impl Display {
    pub fn open(path: &Path) -> Result<Display> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let fd: OwnedFd = file.into();
        // SAFETY: fd is a valid DRM fd kept open as long as the display.
        let dpy = unsafe { sys::vaGetDisplayDRM(fd.as_raw_fd()) };
        if dpy.is_null() {
            bail!("vaGetDisplayDRM({}) failed", path.display());
        }
        // SAFETY: valid display; the callbacks only log.
        unsafe {
            sys::vaSetInfoCallback(dpy, Some(log_info), std::ptr::null_mut());
            sys::vaSetErrorCallback(dpy, Some(log_error), std::ptr::null_mut());
        }
        let (mut major, mut minor) = (0, 0);
        // SAFETY: valid display and out-pointers.
        let st = unsafe { sys::vaInitialize(dpy, &mut major, &mut minor) };
        if let Err(e) = check("vaInitialize", st) {
            unsafe { sys::vaTerminate(dpy) };
            return Err(e).with_context(|| format!("VA-API on {}", path.display()));
        }
        // SAFETY: static string owned by the driver.
        let vendor = unsafe {
            let v = sys::vaQueryVendorString(dpy);
            if v.is_null() {
                String::new()
            } else {
                CStr::from_ptr(v).to_string_lossy().into_owned()
            }
        };
        Ok(Display {
            dpy,
            path: path.to_path_buf(),
            vendor,
            _fd: fd,
        })
    }

    /// DRM render nodes, in order (renderD128 first, as GStreamer's
    /// `vah264enc` picks it).
    pub fn render_nodes() -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("renderD"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    pub fn profiles(&self) -> Vec<sys::VAProfile> {
        // SAFETY: valid display; buffer sized by vaMaxNumProfiles.
        unsafe {
            let n = sys::vaMaxNumProfiles(self.dpy).max(0) as usize;
            let mut v = vec![0; n];
            let mut got = 0;
            if sys::vaQueryConfigProfiles(self.dpy, v.as_mut_ptr(), &mut got) as u32
                != sys::VA_STATUS_SUCCESS
            {
                return Vec::new();
            }
            v.truncate(got.max(0) as usize);
            v
        }
    }

    pub fn entrypoints(&self, profile: sys::VAProfile) -> Vec<sys::VAEntrypoint> {
        // SAFETY: as above.
        unsafe {
            let n = sys::vaMaxNumEntrypoints(self.dpy).max(0) as usize;
            let mut v = vec![0; n];
            let mut got = 0;
            if sys::vaQueryConfigEntrypoints(self.dpy, profile, v.as_mut_ptr(), &mut got) as u32
                != sys::VA_STATUS_SUCCESS
            {
                return Vec::new();
            }
            v.truncate(got.max(0) as usize);
            v
        }
    }

    /// One config attribute, None if not supported.
    pub fn attribute(
        &self,
        profile: sys::VAProfile,
        ep: sys::VAEntrypoint,
        ty: sys::VAConfigAttribType,
    ) -> Option<u32> {
        let mut a = sys::VAConfigAttrib {
            type_: ty,
            value: 0,
        };
        // SAFETY: one attribute in, one out.
        let st = unsafe { sys::vaGetConfigAttributes(self.dpy, profile, ep, &mut a, 1) };
        (st as u32 == sys::VA_STATUS_SUCCESS && a.value != sys::VA_ATTRIB_NOT_SUPPORTED)
            .then_some(a.value)
    }

    pub fn config(
        &self,
        profile: sys::VAProfile,
        ep: sys::VAEntrypoint,
        attrs: &[(sys::VAConfigAttribType, u32)],
    ) -> Result<Config> {
        let mut a: Vec<sys::VAConfigAttrib> = attrs
            .iter()
            .map(|&(type_, value)| sys::VAConfigAttrib { type_, value })
            .collect();
        let mut id = 0;
        // SAFETY: attribute array and out-pointer valid for the call.
        let st = unsafe {
            sys::vaCreateConfig(
                self.dpy,
                profile,
                ep,
                a.as_mut_ptr(),
                a.len() as i32,
                &mut id,
            )
        };
        check("vaCreateConfig", st)?;
        Ok(Config { dpy: self.dpy, id })
    }

    /// `targets`: surfaces the context renders into (the encoder's
    /// reconstructed pictures, as FFmpeg passes them; empty for VPP, as
    /// GStreamer's va plugin does).
    pub fn context(
        &self,
        config: &Config,
        width: u32,
        height: u32,
        targets: &[sys::VASurfaceID],
    ) -> Result<Context> {
        let mut id = 0;
        let mut t = targets.to_vec();
        // SAFETY: the target list is valid for the call.
        let st = unsafe {
            sys::vaCreateContext(
                self.dpy,
                config.id,
                width as i32,
                height as i32,
                sys::VA_PROGRESSIVE as i32,
                if t.is_empty() {
                    std::ptr::null_mut()
                } else {
                    t.as_mut_ptr()
                },
                t.len() as i32,
                &mut id,
            )
        };
        check("vaCreateContext", st)?;
        Ok(Context { dpy: self.dpy, id })
    }

    fn surface_with(
        &self,
        rt_format: u32,
        width: u32,
        height: u32,
        attrs: &mut [sys::VASurfaceAttrib],
        what: &str,
    ) -> Result<Surface> {
        let mut id = 0;
        // SAFETY: one surface; attribute pointers valid for the call.
        let st = unsafe {
            sys::vaCreateSurfaces(
                self.dpy,
                rt_format,
                width,
                height,
                &mut id,
                1,
                attrs.as_mut_ptr(),
                attrs.len() as u32,
            )
        };
        check(what, st)?;
        Ok(Surface {
            dpy: self.dpy,
            id,
            width,
            height,
        })
    }

    /// A surface of `fourcc` (VA fourcc), with an optional usage hint.
    pub fn surface(
        &self,
        rt_format: u32,
        fourcc: u32,
        width: u32,
        height: u32,
        usage: Option<u32>,
    ) -> Result<Surface> {
        let mut attrs = vec![int_attr(sys::VASurfaceAttribPixelFormat, fourcc as i32)];
        if let Some(u) = usage {
            attrs.push(int_attr(sys::VASurfaceAttribUsageHint, u as i32));
        }
        self.surface_with(rt_format, width, height, &mut attrs, "vaCreateSurfaces")
    }

    /// Import a DMA-BUF (no copy).
    pub fn import_prime(
        &self,
        desc: &mut sys::VADRMPRIMESurfaceDescriptor,
        rt_format: u32,
    ) -> Result<Surface> {
        let (w, h) = (desc.width, desc.height);
        let mut attrs = [
            int_attr(
                sys::VASurfaceAttribMemoryType,
                sys::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 as i32,
            ),
            sys::VASurfaceAttrib {
                type_: sys::VASurfaceAttribExternalBufferDescriptor,
                flags: sys::VA_SURFACE_ATTRIB_SETTABLE,
                value: sys::VAGenericValue {
                    type_: sys::VAGenericValueTypePointer,
                    value: sys::_VAGenericValue__bindgen_ty_1 {
                        p: desc as *mut _ as *mut c_void,
                    },
                },
            },
        ];
        self.surface_with(
            rt_format,
            w,
            h,
            &mut attrs,
            "vaCreateSurfaces (DMA-BUF import)",
        )
    }

    /// The DRM modifier the driver uses for a surface of `fourcc`, made with
    /// the VPP-input usage hint and exported. What GStreamer's va plugin
    /// offers per format (gstvaallocator.c).
    pub fn probe_modifier(&self, fourcc: u32) -> Option<u64> {
        let s = self
            .surface(
                sys::VA_RT_FORMAT_RGB32,
                fourcc,
                64,
                64,
                Some(sys::VA_SURFACE_ATTRIB_USAGE_HINT_VPP_READ),
            )
            .ok()?;
        let mut desc = sys::VADRMPRIMESurfaceDescriptor::default();
        // SAFETY: valid surface; descriptor written by the driver.
        let st = unsafe {
            sys::vaExportSurfaceHandle(
                self.dpy,
                s.id,
                sys::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                sys::VA_EXPORT_SURFACE_READ_ONLY | sys::VA_EXPORT_SURFACE_COMPOSED_LAYERS,
                &mut desc as *mut _ as *mut c_void,
            )
        };
        if st as u32 != sys::VA_STATUS_SUCCESS {
            return None;
        }
        for o in &desc.objects[..desc.num_objects.min(4) as usize] {
            // SAFETY: the export handed us these fds.
            unsafe { libc::close(o.fd) };
        }
        (desc.num_objects > 0).then(|| desc.objects[0].drm_format_modifier)
    }

    pub fn buffer(
        &self,
        ctx: &Context,
        ty: sys::VABufferType,
        data: &[u8],
        what: &str,
    ) -> Result<Buffer> {
        let mut id = 0;
        // SAFETY: libva copies `data` into the new buffer.
        let st = unsafe {
            sys::vaCreateBuffer(
                self.dpy,
                ctx.id,
                ty,
                data.len() as u32,
                1,
                data.as_ptr() as *mut c_void,
                &mut id,
            )
        };
        check(what, st)?;
        Ok(Buffer { dpy: self.dpy, id })
    }

    /// A parameter struct as a VA buffer.
    pub fn param<T: Copy>(&self, ctx: &Context, ty: sys::VABufferType, v: &T) -> Result<Buffer> {
        // SAFETY: T is a plain C struct from the bindings.
        let bytes = unsafe {
            std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>())
        };
        self.buffer(ctx, ty, bytes, "vaCreateBuffer")
    }

    /// `VAEncMiscParameterBuffer` header + payload.
    pub fn misc<T: Copy>(
        &self,
        ctx: &Context,
        ty: sys::VAEncMiscParameterType,
        v: &T,
    ) -> Result<Buffer> {
        let mut bytes = ty.to_ne_bytes().to_vec();
        // SAFETY: as above.
        bytes.extend_from_slice(unsafe {
            std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>())
        });
        self.buffer(
            ctx,
            sys::VAEncMiscParameterBufferType,
            &bytes,
            "vaCreateBuffer (misc)",
        )
    }

    pub fn coded_buffer(&self, ctx: &Context, size: usize) -> Result<Buffer> {
        let mut id = 0;
        // SAFETY: no initial data.
        let st = unsafe {
            sys::vaCreateBuffer(
                self.dpy,
                ctx.id,
                sys::VAEncCodedBufferType,
                size as u32,
                1,
                std::ptr::null_mut(),
                &mut id,
            )
        };
        check("vaCreateBuffer (coded)", st)?;
        Ok(Buffer { dpy: self.dpy, id })
    }

    /// Render `buffers` into `target` with `ctx` and wait for the result.
    pub fn run(&self, ctx: &Context, target: &Surface, buffers: &[Buffer]) -> Result<()> {
        let mut ids: Vec<sys::VABufferID> = buffers.iter().map(|b| b.id).collect();
        // SAFETY: valid context, surface and buffer ids.
        unsafe {
            check(
                "vaBeginPicture",
                sys::vaBeginPicture(self.dpy, ctx.id, target.id),
            )?;
            let st = sys::vaRenderPicture(self.dpy, ctx.id, ids.as_mut_ptr(), ids.len() as i32);
            let end = sys::vaEndPicture(self.dpy, ctx.id);
            check("vaRenderPicture", st)?;
            check("vaEndPicture", end)?;
            check("vaSyncSurface", sys::vaSyncSurface(self.dpy, target.id))?;
        }
        Ok(())
    }

    /// The encoded bytes of a coded buffer (all segments).
    pub fn read_coded(&self, b: &Buffer) -> Result<Vec<u8>> {
        let mut p: *mut c_void = std::ptr::null_mut();
        // SAFETY: mapping a coded buffer yields a list of segments, valid
        // until unmapped.
        unsafe {
            check(
                "vaMapBuffer (coded)",
                sys::vaMapBuffer(self.dpy, b.id, &mut p),
            )?;
            let mut out = Vec::new();
            let mut seg = p as *const sys::VACodedBufferSegment;
            while !seg.is_null() {
                let s = &*seg;
                if s.size > 0 && !s.buf.is_null() {
                    out.extend_from_slice(std::slice::from_raw_parts(
                        s.buf as *const u8,
                        s.size as usize,
                    ));
                }
                seg = s.next as *const sys::VACodedBufferSegment;
            }
            sys::vaUnmapBuffer(self.dpy, b.id);
            Ok(out)
        }
    }

    /// Copy CPU pixels (one plane, `stride` bytes per row) into a surface
    /// of the same size: vaDeriveImage, or vaCreateImage + vaPutImage.
    pub fn upload(&self, s: &Surface, fourcc: u32, data: &[u8], stride: usize) -> Result<()> {
        let row = s.width as usize * 4;
        let copy = |dst: *mut u8, pitch: usize| {
            for y in 0..s.height as usize {
                let src = &data[y * stride..y * stride + row];
                // SAFETY: the mapped image holds `height` rows of `pitch`.
                unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), dst.add(y * pitch), row) };
            }
        };
        if data.len() < stride * (s.height as usize - 1) + row {
            bail!("frame shorter than its stride says");
        }
        let mut img = sys::VAImage::default();
        // SAFETY: valid surface; the image is destroyed below.
        unsafe {
            if sys::vaDeriveImage(self.dpy, s.id, &mut img) as u32 == sys::VA_STATUS_SUCCESS {
                let r = self.with_mapped(img.buf, |p| {
                    copy(p.add(img.offsets[0] as usize), img.pitches[0] as usize)
                });
                sys::vaDestroyImage(self.dpy, img.image_id);
                return r;
            }
            // Fallback: an image in the driver's own format, then put.
            let n = sys::vaMaxNumImageFormats(self.dpy).max(0) as usize;
            let mut fmts = vec![sys::VAImageFormat::default(); n];
            let mut got = 0;
            check(
                "vaQueryImageFormats",
                sys::vaQueryImageFormats(self.dpy, fmts.as_mut_ptr(), &mut got),
            )?;
            let mut fmt = *fmts[..got.max(0) as usize]
                .iter()
                .find(|f| f.fourcc == fourcc)
                .ok_or_else(|| anyhow!("the VA driver has no image format for the frame"))?;
            check(
                "vaCreateImage",
                sys::vaCreateImage(
                    self.dpy,
                    &mut fmt,
                    s.width as i32,
                    s.height as i32,
                    &mut img,
                ),
            )?;
            let r = self
                .with_mapped(img.buf, |p| {
                    copy(p.add(img.offsets[0] as usize), img.pitches[0] as usize)
                })
                .and_then(|_| {
                    check(
                        "vaPutImage",
                        sys::vaPutImage(
                            self.dpy,
                            s.id,
                            img.image_id,
                            0,
                            0,
                            s.width,
                            s.height,
                            0,
                            0,
                            s.width,
                            s.height,
                        ),
                    )
                });
            sys::vaDestroyImage(self.dpy, img.image_id);
            r
        }
    }

    fn with_mapped(&self, buf: sys::VABufferID, f: impl FnOnce(*mut u8)) -> Result<()> {
        let mut p: *mut c_void = std::ptr::null_mut();
        // SAFETY: valid buffer id; unmapped right after use.
        unsafe {
            check(
                "vaMapBuffer (image)",
                sys::vaMapBuffer(self.dpy, buf, &mut p),
            )?;
            f(p as *mut u8);
            sys::vaUnmapBuffer(self.dpy, buf);
        }
        Ok(())
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // SAFETY: every object made from it is gone (dropped first).
        unsafe { sys::vaTerminate(self.dpy) };
    }
}

fn int_attr(ty: sys::VASurfaceAttribType, v: i32) -> sys::VASurfaceAttrib {
    sys::VASurfaceAttrib {
        type_: ty,
        flags: sys::VA_SURFACE_ATTRIB_SETTABLE,
        value: sys::VAGenericValue {
            type_: sys::VAGenericValueTypeInteger,
            value: sys::_VAGenericValue__bindgen_ty_1 { i: v },
        },
    }
}

pub struct Config {
    dpy: sys::VADisplay,
    pub id: sys::VAConfigID,
}

impl Drop for Config {
    fn drop(&mut self) {
        unsafe { sys::vaDestroyConfig(self.dpy, self.id) };
    }
}

pub struct Context {
    dpy: sys::VADisplay,
    pub id: sys::VAContextID,
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { sys::vaDestroyContext(self.dpy, self.id) };
    }
}

pub struct Surface {
    dpy: sys::VADisplay,
    pub id: sys::VASurfaceID,
    pub width: u32,
    pub height: u32,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe { sys::vaDestroySurfaces(self.dpy, &mut self.id, 1) };
    }
}

/// A parameter or coded buffer. Parameter buffers are destroyed after
/// vaEndPicture (as FFmpeg does; libva 2 drivers don't free them).
pub struct Buffer {
    dpy: sys::VADisplay,
    pub id: sys::VABufferID,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe { sys::vaDestroyBuffer(self.dpy, self.id) };
    }
}
