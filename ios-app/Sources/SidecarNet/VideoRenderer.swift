// Phase 2, Metal renderer: draws a decoded NV12 picture (two planes, mapped
// into Metal textures without a copy) aspect-fit into any BGRA texture,
// converting YCbCr → RGB in the fragment shader. Platform-neutral (Metal +
// CoreVideo), so the same code renders offscreen in the macOS tests and
// into the CAMetalLayer drawable on the iPad.
import CoreVideo
import Foundation
import Metal
import simd

public enum YCbCrMatrix: Equatable, Sendable {
    case bt601, bt709, bt2020
}

/// rgb = matrix × ((Y, Cb, Cr) − offset), every value in 0…1.
public struct YCbCrToRGB {
    public var matrix: simd_float3x3
    public var offset: SIMD3<Float>

    public init(_ m: YCbCrMatrix, fullRange: Bool) {
        let k: (r: Float, b: Float)
        switch m {
        case .bt601: k = (0.299, 0.114)
        case .bt709: k = (0.2126, 0.0722)
        case .bt2020: k = (0.2627, 0.0593)
        }
        let kr = k.r, kb = k.b
        let kg = 1 - kr - kb
        // Video range: Y 16–235 and Cb/Cr 16–240 (8-bit) stretched to 0–1.
        let ys: Float = fullRange ? 1 : 255.0 / 219.0
        let cs: Float = fullRange ? 1 : 255.0 / 224.0
        offset = SIMD3(fullRange ? 0 : 16.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0)
        matrix = simd_float3x3(columns: (
            SIMD3(ys, ys, ys),                                          // × Y
            SIMD3(0, -2 * kb * (1 - kb) / kg * cs, 2 * (1 - kb) * cs),  // × Cb
            SIMD3(2 * (1 - kr) * cs, -2 * kr * (1 - kr) / kg * cs, 0)   // × Cr
        ))
    }

    /// What the decoder attached to the picture (from the stream's VUI), or
    /// BT.709 for HD / BT.601 for SD when the stream doesn't say.
    public static func forImage(_ image: CVPixelBuffer) -> YCbCrToRGB {
        let fullRange = CVPixelBufferGetPixelFormatType(image) == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
        let attachments = CVBufferCopyAttachments(image, .shouldPropagate) as? [String: Any]
        let key = attachments?[kCVImageBufferYCbCrMatrixKey as String] as? String
        let m: YCbCrMatrix
        if key == (kCVImageBufferYCbCrMatrix_ITU_R_601_4 as String) {
            m = .bt601
        } else if key == (kCVImageBufferYCbCrMatrix_ITU_R_709_2 as String) {
            m = .bt709
        } else if key == (kCVImageBufferYCbCrMatrix_ITU_R_2020 as String) {
            m = .bt2020
        } else {
            m = CVPixelBufferGetHeight(image) >= 720 ? .bt709 : .bt601
        }
        return YCbCrToRGB(m, fullRange: fullRange)
    }

    /// The same conversion on the CPU (tests compare it with the shader).
    public func rgb(y: UInt8, cb: Float, cr: Float) -> SIMD3<Float> {
        let v = SIMD3<Float>(Float(y) / 255, cb / 255, cr / 255) - offset
        return simd_clamp(matrix * v, SIMD3(repeating: 0), SIMD3(repeating: 1))
    }
}

/// Where a `content` picture lands when shown aspect-fit and centred in a
/// `target` of another shape, in clip space: (left, top, right, bottom),
/// (-1, 1, 1, -1) being the whole target.
public func aspectFitRect(contentWidth cw: Double, contentHeight ch: Double,
                          targetWidth tw: Double, targetHeight th: Double) -> SIMD4<Float> {
    guard cw > 0, ch > 0, tw > 0, th > 0 else { return SIMD4(-1, 1, 1, -1) }
    let s = min(tw / cw, th / ch)
    let sx = Float(cw * s / tw), sy = Float(ch * s / th)
    return SIMD4(-sx, sy, sx, -sy)
}

public enum VideoRendererError: Error {
    case noCommandQueue
    case textureCache(CVReturn)
    case texture(plane: Int, CVReturn)
    case notNV12(OSType)
    case encoder
}

public final class VideoRenderer: @unchecked Sendable {
    public let device: MTLDevice
    public let commandQueue: MTLCommandQueue
    public let pixelFormat: MTLPixelFormat
    private let pipeline: MTLRenderPipelineState
    private let textureCache: CVMetalTextureCache

    /// Layout of `Params` in the shader.
    struct Params {
        var rect: SIMD4<Float>
        var c0: SIMD4<Float>
        var c1: SIMD4<Float>
        var c2: SIMD4<Float>
        var offset: SIMD4<Float>
    }

    // Compiled at runtime: SwiftPM (and so xtool) doesn't build .metal files.
    static let shaderSource = """
    #include <metal_stdlib>
    using namespace metal;

    struct Params {
        float4 rect;    // left, top, right, bottom in clip space
        float4 c0;      // YCbCr -> RGB matrix columns
        float4 c1;
        float4 c2;
        float4 offset;  // subtracted from (Y, Cb, Cr) first
    };

    struct VOut {
        float4 position [[position]];
        float2 uv;
    };

    vertex VOut sidecar_video_vertex(uint vid [[vertex_id]], constant Params &p [[buffer(0)]]) {
        float2 uv = float2(float(vid & 1), float(vid >> 1));
        VOut o;
        o.position = float4(mix(p.rect.x, p.rect.z, uv.x), mix(p.rect.y, p.rect.w, uv.y), 0.0, 1.0);
        o.uv = uv;
        return o;
    }

    fragment float4 sidecar_video_fragment(VOut in [[stage_in]],
                                           texture2d<float> luma [[texture(0)]],
                                           texture2d<float> chroma [[texture(1)]],
                                           constant Params &p [[buffer(0)]]) {
        constexpr sampler s(coord::normalized, filter::linear, address::clamp_to_edge);
        float3 ycc = float3(luma.sample(s, in.uv).r, chroma.sample(s, in.uv).rg) - p.offset.xyz;
        float3 rgb = float3x3(p.c0.xyz, p.c1.xyz, p.c2.xyz) * ycc;
        return float4(saturate(rgb), 1.0);
    }
    """

    public init(device: MTLDevice, pixelFormat: MTLPixelFormat = .bgra8Unorm) throws {
        self.device = device
        self.pixelFormat = pixelFormat
        let library = try device.makeLibrary(source: Self.shaderSource, options: nil)
        let desc = MTLRenderPipelineDescriptor()
        desc.vertexFunction = library.makeFunction(name: "sidecar_video_vertex")
        desc.fragmentFunction = library.makeFunction(name: "sidecar_video_fragment")
        desc.colorAttachments[0].pixelFormat = pixelFormat
        pipeline = try device.makeRenderPipelineState(descriptor: desc)
        guard let q = device.makeCommandQueue() else { throw VideoRendererError.noCommandQueue }
        commandQueue = q
        var cache: CVMetalTextureCache?
        let st = CVMetalTextureCacheCreate(kCFAllocatorDefault, nil, device, nil, &cache)
        guard st == kCVReturnSuccess, let cache else { throw VideoRendererError.textureCache(st) }
        textureCache = cache
    }

    /// A command buffer that draws `image` aspect-fit (black bars) into
    /// `target`. The caller adds the present and commits.
    public func encode(_ image: CVPixelBuffer, into target: MTLTexture) throws -> MTLCommandBuffer {
        let fmt = CVPixelBufferGetPixelFormatType(image)
        guard fmt == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
                || fmt == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange else {
            throw VideoRendererError.notNV12(fmt)
        }
        let lumaRef = try planeTexture(image, plane: 0, format: .r8Unorm)
        let chromaRef = try planeTexture(image, plane: 1, format: .rg8Unorm)
        guard let luma = CVMetalTextureGetTexture(lumaRef), let chroma = CVMetalTextureGetTexture(chromaRef),
              let cb = commandQueue.makeCommandBuffer() else { throw VideoRendererError.encoder }

        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = target
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)
        pass.colorAttachments[0].storeAction = .store
        guard let enc = cb.makeRenderCommandEncoder(descriptor: pass) else { throw VideoRendererError.encoder }
        let conv = YCbCrToRGB.forImage(image)
        var p = Params(
            rect: aspectFitRect(contentWidth: Double(CVPixelBufferGetWidth(image)),
                                contentHeight: Double(CVPixelBufferGetHeight(image)),
                                targetWidth: Double(target.width), targetHeight: Double(target.height)),
            c0: SIMD4(conv.matrix.columns.0, 0), c1: SIMD4(conv.matrix.columns.1, 0),
            c2: SIMD4(conv.matrix.columns.2, 0), offset: SIMD4(conv.offset, 0))
        enc.setRenderPipelineState(pipeline)
        enc.setVertexBytes(&p, length: MemoryLayout<Params>.stride, index: 0)
        enc.setFragmentBytes(&p, length: MemoryLayout<Params>.stride, index: 0)
        enc.setFragmentTexture(luma, index: 0)
        enc.setFragmentTexture(chroma, index: 1)
        enc.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
        enc.endEncoding()
        // The CoreVideo textures (and so the decoder's IOSurface) must stay
        // alive until the GPU is done reading them.
        cb.addCompletedHandler { _ in withExtendedLifetime((lumaRef, chromaRef)) {} }
        return cb
    }

    private func planeTexture(_ image: CVPixelBuffer, plane: Int, format: MTLPixelFormat) throws -> CVMetalTexture {
        var tex: CVMetalTexture?
        let st = CVMetalTextureCacheCreateTextureFromImage(
            kCFAllocatorDefault, textureCache, image, nil, format,
            CVPixelBufferGetWidthOfPlane(image, plane), CVPixelBufferGetHeightOfPlane(image, plane), plane, &tex)
        guard st == kCVReturnSuccess, let tex else { throw VideoRendererError.texture(plane: plane, st) }
        return tex
    }
}
