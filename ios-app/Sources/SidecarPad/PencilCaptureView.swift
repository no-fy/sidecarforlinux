#if canImport(UIKit)
import SidecarProtocol
import SidecarNet
import UIKit

@MainActor
protocol PencilCaptureDelegate: AnyObject {
    func capture(_ view: PencilCaptureView, batch: PenBatch)
    func capture(_ view: PencilCaptureView, gesture: PencilGesture)
}

/// Full-screen surface that turns every Apple Pencil signal into protocol
/// samples:
/// * contact: every 240 Hz sample via `coalescedTouches(for:)`, optional
///   `predictedTouches(for:)` (flagged), force, altitude, azimuth, roll;
/// * hover: `UIHoverGestureRecognizer` (zOffset, angles, roll);
/// * double-tap / squeeze: `UIPencilInteraction` (iOS 17.5 API);
/// * Pencil Pro haptics: `UICanvasFeedbackGenerator`.
/// Finger touches are ignored (palm rejection by construction).
final class PencilCaptureView: UIView, UIPencilInteractionDelegate {
    weak var delegate: PencilCaptureDelegate?
    var sendPredicted = false
    var showInk = true {
        // Drop the last stroke, or `end()` would flash it again later.
        didSet { if !showInk { ink.clear() } }
    }

    private var seq: UInt32 = 0
    private var touching = false
    private var hovering = false
    private var pendingOut: DispatchWorkItem?
    private var lastLocation: CGPoint?
    private lazy var feedback = UICanvasFeedbackGenerator(view: self)
    private let ink = InkLayer()
    private let cursor = CAShapeLayer()

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        backgroundColor = UIColor(white: 0.08, alpha: 1)

        let hover = UIHoverGestureRecognizer(target: self, action: #selector(hovered(_:)))
        hover.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.pencil.rawValue)]
        addGestureRecognizer(hover)

        let pencil = UIPencilInteraction(delegate: self)
        addInteraction(pencil)

        layer.addSublayer(ink)
        cursor.path = UIBezierPath(ovalIn: CGRect(x: -6, y: -6, width: 12, height: 12)).cgPath
        cursor.strokeColor = UIColor.systemTeal.cgColor
        cursor.fillColor = UIColor.clear.cgColor
        cursor.lineWidth = 1.5
        cursor.isHidden = true
        layer.addSublayer(cursor)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func layoutSubviews() {
        super.layoutSubviews()
        ink.frame = bounds
    }

    // MARK: sample construction

    private func norm(_ p: CGPoint) -> (Float, Float) {
        let w = max(bounds.width, 1)
        let h = max(bounds.height, 1)
        return (Float(p.x / w), Float(p.y / h))
    }

    private func nextSeq() -> UInt32 {
        seq &+= 1
        return seq
    }

    private func sample(from t: UITouch, phase: PenPhase, extraFlags: UInt8) -> PenSample {
        let loc = t.preciseLocation(in: self)
        let (x, y) = norm(loc)
        var flags = extraFlags | SampleFlags.rollValid
        let pending = t.estimatedPropertiesExpectingUpdates
        if pending.contains(.force) { flags |= SampleFlags.pressureEstimated }
        if pending.contains(.altitude) || pending.contains(.azimuth) { flags |= SampleFlags.anglesEstimated }
        let maxForce = t.maximumPossibleForce
        let pressure = maxForce > 0 ? Float(min(max(t.force / maxForce, 0), 1)) : 0
        return PenSample(
            timestampNs: SessionClock.ns(fromUptime: t.timestamp),
            seq: nextSeq(),
            phase: phase,
            flags: flags,
            x: x, y: y,
            pressure: phase == .up || phase == .cancel ? 0 : pressure,
            altitude: Float(t.altitudeAngle),
            azimuth: Float(t.azimuthAngle(in: self)),
            roll: Float(t.rollAngle),
            zOffset: 0,
            estimationIndex: t.estimationUpdateIndex?.uint32Value ?? 0
        )
    }

    private func emit(_ samples: [PenSample]) {
        guard !samples.isEmpty else { return }
        delegate?.capture(self, batch: PenBatch(sendTimeNs: SessionClock.nowNs(), samples: samples))
    }

    private func pencilTouch(_ touches: Set<UITouch>) -> UITouch? {
        touches.first { $0.type == .pencil }
    }

    // MARK: contact

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let t = pencilTouch(touches) else { return }
        pendingOut?.cancel()
        touching = true
        cursor.isHidden = true
        let coalesced = event?.coalescedTouches(for: t) ?? [t]
        var out: [PenSample] = []
        for (i, c) in coalesced.enumerated() {
            out.append(sample(from: c, phase: i == 0 ? .down : .move,
                              extraFlags: coalesced.count > 1 ? SampleFlags.coalesced : 0))
        }
        emit(out)
        lastLocation = t.location(in: self)
        if showInk { ink.begin(at: t.location(in: self), force: t.force) }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let t = pencilTouch(touches), touching else { return }
        let coalesced = event?.coalescedTouches(for: t) ?? [t]
        var out = coalesced.map { sample(from: $0, phase: .move, extraFlags: SampleFlags.coalesced) }
        if sendPredicted, let predicted = event?.predictedTouches(for: t) {
            out += predicted.map { sample(from: $0, phase: .move, extraFlags: SampleFlags.predicted) }
        }
        emit(out)
        lastLocation = t.location(in: self)
        if showInk { coalesced.forEach { ink.add($0.location(in: self), force: $0.force) } }
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        finish(touches, event: event, phase: .up)
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        finish(touches, event: event, phase: .cancel)
    }

    private func finish(_ touches: Set<UITouch>, event: UIEvent?, phase: PenPhase) {
        guard let t = pencilTouch(touches), touching else { return }
        touching = false
        let coalesced = event?.coalescedTouches(for: t) ?? [t]
        var out: [PenSample] = []
        for (i, c) in coalesced.enumerated() {
            out.append(sample(from: c, phase: i == coalesced.count - 1 ? phase : .move,
                              extraFlags: SampleFlags.coalesced))
        }
        emit(out)
        // Only fade ink we drew: `end()` animates opacity from 1, which would
        // flash the last stroke from when local ink was still on.
        if showInk { ink.end() }
        // If no hover follows (pencil lifted fast, out of hover range), leave
        // proximity shortly after the lift.
        scheduleOut(after: 0.15)
    }

    // MARK: hover

    @objc private func hovered(_ g: UIHoverGestureRecognizer) {
        switch g.state {
        case .began, .changed:
            guard !touching else { return }
            pendingOut?.cancel()
            hovering = true
            let loc = g.location(in: self)
            let (x, y) = norm(loc)
            let s = PenSample(
                timestampNs: SessionClock.nowNs(),
                seq: nextSeq(),
                phase: .hover,
                flags: SampleFlags.fromHover | SampleFlags.rollValid,
                x: x, y: y,
                pressure: 0,
                altitude: Float(g.altitudeAngle),
                azimuth: Float(g.azimuthAngle(in: self)),
                roll: Float(g.rollAngle),
                zOffset: Float(g.zOffset),
                estimationIndex: 0
            )
            emit([s])
            lastLocation = loc
            cursor.isHidden = false
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            cursor.position = loc
            CATransaction.commit()
        case .ended, .cancelled, .failed:
            hovering = false
            cursor.isHidden = true
            // Touching down ends hover; wait a moment so a stroke start
            // doesn't flicker proximity out/in.
            scheduleOut(after: 0.06)
        default:
            break
        }
    }

    private func scheduleOut(after delay: TimeInterval) {
        pendingOut?.cancel()
        let item = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self, !self.touching, !self.hovering else { return }
                let (x, y) = self.norm(self.lastLocation ?? .zero)
                let s = PenSample(timestampNs: SessionClock.nowNs(), seq: self.nextSeq(), phase: .out, flags: 0,
                                  x: x, y: y, pressure: 0, altitude: .pi / 2, azimuth: 0, roll: 0, zOffset: 1,
                                  estimationIndex: 0)
                self.emit([s])
            }
        }
        pendingOut = item
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: item)
    }

    /// Called when the app loses focus: never leave the PC with a pen down.
    func forceOut() {
        touching = false
        hovering = false
        pendingOut?.cancel()
        let (x, y) = norm(lastLocation ?? .zero)
        emit([PenSample(timestampNs: SessionClock.nowNs(), seq: nextSeq(), phase: .out, flags: 0, x: x, y: y,
                        pressure: 0, altitude: .pi / 2, azimuth: 0, roll: 0, zOffset: 1, estimationIndex: 0)])
    }

    // MARK: double-tap / squeeze (iOS 17.5)

    private func pose(_ p: UIPencilHoverPose?) -> HoverPose? {
        guard let p else { return nil }
        let (x, y) = norm(p.location)
        return HoverPose(x: x, y: y, zOffset: Float(p.zOffset), altitude: Float(p.altitudeAngle),
                         azimuth: Float(p.azimuthAngle), roll: Float(p.rollAngle))
    }

    func pencilInteraction(_ interaction: UIPencilInteraction, didReceiveTap tap: UIPencilInteraction.Tap) {
        let g = PencilGesture(timestampNs: SessionClock.ns(fromUptime: tap.timestamp), kind: .doubleTap,
                              phase: .discrete,
                              preferredAction: UInt8(truncatingIfNeeded: UIPencilInteraction.preferredTapAction.rawValue),
                              hoverPose: pose(tap.hoverPose))
        delegate?.capture(self, gesture: g)
    }

    func pencilInteraction(_ interaction: UIPencilInteraction, didReceiveSqueeze squeeze: UIPencilInteraction.Squeeze) {
        let phase: GesturePhase
        switch squeeze.phase {
        case .began: phase = .began
        case .changed: phase = .changed
        case .ended: phase = .ended
        case .cancelled: phase = .cancelled
        @unknown default: phase = .cancelled
        }
        let g = PencilGesture(timestampNs: SessionClock.ns(fromUptime: squeeze.timestamp), kind: .squeeze,
                              phase: phase,
                              preferredAction: UInt8(truncatingIfNeeded:
                                                        UIPencilInteraction.preferredSqueezeAction.rawValue),
                              hoverPose: pose(squeeze.hoverPose))
        delegate?.capture(self, gesture: g)
    }

    // MARK: haptics (Pencil Pro)

    func playHaptic(_ h: Haptic) {
        let point: CGPoint
        if h.x.isNaN || h.y.isNaN {
            point = lastLocation ?? CGPoint(x: bounds.midX, y: bounds.midY)
        } else {
            point = CGPoint(x: CGFloat(h.x) * bounds.width, y: CGFloat(h.y) * bounds.height)
        }
        switch h.kind {
        case .alignment: feedback.alignmentOccurred(at: point)
        case .pathCompleted: feedback.pathCompleted(at: point)
        }
    }
}

/// Short-lived local ink so the user sees where the pencil went (the PC's
/// screen is the real canvas; phase 2 shows it on the iPad).
final class InkLayer: CAShapeLayer {
    private var bezier = UIBezierPath()

    override init() {
        super.init()
        strokeColor = UIColor.systemTeal.withAlphaComponent(0.8).cgColor
        fillColor = UIColor.clear.cgColor
        lineWidth = 2
        lineCap = .round
        lineJoin = .round
    }

    override init(layer: Any) { super.init(layer: layer) }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func begin(at p: CGPoint, force: CGFloat) {
        removeAllAnimations()
        opacity = 1
        bezier = UIBezierPath()
        bezier.move(to: p)
        update()
    }

    func add(_ p: CGPoint, force: CGFloat) {
        bezier.addLine(to: p)
        update()
    }

    func end() {
        let fade = CABasicAnimation(keyPath: "opacity")
        fade.fromValue = 1
        fade.toValue = 0
        fade.duration = 0.8
        fade.isRemovedOnCompletion = false
        fade.fillMode = .forwards
        add(fade, forKey: "fade")
    }

    func clear() {
        removeAllAnimations()
        bezier = UIBezierPath()
        update()
    }

    private func update() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        path = bezier.cgPath
        CATransaction.commit()
    }
}
#endif
