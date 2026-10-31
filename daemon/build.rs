//! Generates libva bindings for the native VA-API encoder (Linux, cargo
//! feature `native-video`). Needs the libva headers (Arch: `libva`, Debian/
//! Ubuntu: `libva-dev`) and libclang (Arch: `clang`, Debian/Ubuntu:
//! `libclang-dev`). Other builds skip this entirely.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "native-video")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        va::generate();
    }
}

#[cfg(feature = "native-video")]
mod va {
    use std::path::PathBuf;

    /// Only what native/va.rs uses: display, config, surfaces (incl. DRM
    /// PRIME import/export), context, buffers, pictures, images, and the
    /// H.264 encode + video processing parameter structures.
    const FUNCTIONS: &str = "va(GetDisplayDRM|Initialize|Terminate|ErrorStr|QueryVendorString|\
        QueryConfigProfiles|QueryConfigEntrypoints|GetConfigAttributes|CreateConfig|DestroyConfig|\
        CreateSurfaces|DestroySurfaces|CreateContext|DestroyContext|CreateBuffer|DestroyBuffer|\
        MapBuffer|UnmapBuffer|BeginPicture|RenderPicture|EndPicture|SyncSurface|\
        ExportSurfaceHandle|DeriveImage|DestroyImage|CreateImage|PutImage|QueryImageFormats|\
        MaxNumImageFormats|MaxNumProfiles|MaxNumEntrypoints|\
        SetErrorCallback|SetInfoCallback)";
    const TYPES: &str = "VAEncSequenceParameterBufferH264|VAEncPictureParameterBufferH264|\
        VAEncSliceParameterBufferH264|VAPictureH264|VAEncMiscParameterBuffer|\
        VAEncMiscParameterRateControl|VAEncMiscParameterHRD|VAEncMiscParameterFrameRate|\
        VAEncMiscParameterBufferQualityLevel|VAEncPackedHeaderParameterBuffer|\
        VAProcPipelineParameterBuffer|VARectangle|VADRMPRIMESurfaceDescriptor|VASurfaceAttrib|\
        VACodedBufferSegment|VAImage|VAImageFormat|VAConfigAttrib|VAProcColorProperties|\
        VAEncPackedHeaderType";

    pub fn generate() {
        let lib = pkg_config::Config::new()
            .atleast_version("1.0")
            .probe("libva-drm")
            .unwrap_or_else(|e| {
                panic!(
                    "libva-drm not found ({e}). Install the libva headers (Arch: libva, \
                     Debian/Ubuntu: libva-dev) or build without native video: \
                     cargo build --no-default-features --features display"
                )
            });
        let mut b = bindgen::Builder::default()
            .header_contents(
                "va_wrapper.h",
                "#include <va/va.h>\n#include <va/va_drm.h>\n#include <va/va_drmcommon.h>\n",
            )
            .allowlist_function(FUNCTIONS.replace(char::is_whitespace, ""))
            .allowlist_type(TYPES.replace(char::is_whitespace, ""))
            .allowlist_var("VA_.*")
            .allowlist_var("VA[A-Z][A-Za-z0-9]*")
            // Our MSRV: no `unsafe extern` blocks (Rust 1.82).
            .rust_target(bindgen::RustTarget::stable(80, 0).expect("Rust 1.80 target"))
            .default_enum_style(bindgen::EnumVariation::Consts)
            .prepend_enum_name(false)
            .derive_default(true)
            .layout_tests(false)
            .generate_comments(false);
        for p in &lib.include_paths {
            b = b.clang_arg(format!("-I{}", p.display()));
        }
        let bindings = b.generate().unwrap_or_else(|e| {
            panic!("generating libva bindings failed ({e}); is libclang installed (Arch: clang)?")
        });
        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("va.rs");
        bindings.write_to_file(out).expect("writing va.rs");
        println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    }
}
