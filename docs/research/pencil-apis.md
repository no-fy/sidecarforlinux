# Apple Pencil Pro and related iPadOS APIs: verified against Apple docs (2026-10-30)

Method: I read every signature and availability value below from Apple's live DocC JSON
(`https://developer.apple.com/tutorials/data/documentation/<path>.json`, which shows the same content as
`https://developer.apple.com/documentation/<path>`), or from Apple's release notes, tech notes and WWDC
transcripts. Lines marked **[inference]** or **[third-party]** are not stated by Apple. Treat them as hypotheses to check on a device.

---

## 0. Platform status: the "iPadOS 26 / Xcode 26" premise is out of date

* **iPadOS 27 and Xcode 27 are the current releases.** The Xcode release-notes index lists Xcode 27, 27.1 and 27.2. iOS & iPadOS 27 release notes exist. Third-party reports date the public release to 2026-09-14 and iPadOS 27.0.1 to 2026-09-28. Xcode 27.2 and iOS 27.2 are in beta now (the 27.2 notes mention "earlier macOS 27.2 betas").
  * Xcode 27: "includes Swift 6.4 and SDKs for iOS 27, iPadOS 27 … supports on-device debugging in iOS 17 and later … requires a Mac running macOS Tahoe 26.6 or later." Xcode 27 also runs only on Apple-silicon Macs.
    https://developer.apple.com/documentation/xcode-release-notes/xcode-27-release-notes
* **App Store SDK floor:** "Since April 28, 2026 — Apps uploaded to App Store Connect must be built with Xcode 26 or later using an SDK for iOS 26 …". So Xcode 26 is still accepted today. Press reports Xcode 27 becoming mandatory around April 2027 **[third-party]**.
  https://developer.apple.com/news/upcoming-requirements/
* A deployment target of 17.5 works with both toolchains. 17.5 is also the minimum OS for Apple Pencil Pro: per support.apple.com/en-us/108937, Pencil Pro needs iPadOS 17.5 or later on iPad Pro M4/M5, iPad Air M2/M3/M4 or iPad mini A17 Pro.
* **No new raw Pencil input APIs shipped in iPadOS 26 or 27.** The last Pencil API wave was iOS 17.5 (May 2024: roll, squeeze, hover pose, canvas haptics), plus `UIUpdateLink` for low-latency drawing in iOS 18 (June 2024). Source: "Apple Pencil updates", https://developer.apple.com/documentation/updates/applepencil. iOS 27 PencilKit additions (handwriting recognition, stroke ids, Bézier conversion) don't affect raw `UITouch` capture (WWDC26 session 203).

---

## 1. UITouch / UIEvent (pencil contact samples)

| API | Exact Swift declaration | Introduced | Notes |
|---|---|---|---|
| `UITouch` | `@MainActor class UITouch` | iOS 2.0 | Conforms to `Sendable` because it's MainActor-isolated. All properties are main-actor only. |
| `UIEvent.coalescedTouches(for:)` | `func coalescedTouches(for touch: UITouch) -> [UITouch]?` | iOS 9.0 | Includes a copy of the delivered touch as the last element. Returns `nil` if the touch isn't part of this event. |
| `UIEvent.predictedTouches(for:)` | `func predictedTouches(for touch: UITouch) -> [UITouch]?` | iOS 9.0 | Excludes the original touch. Temporary data: drop on the next event. |
| `force` | `var force: CGFloat { get }` | iOS 9.0 | Measured along the pencil axis. Use `altitudeAngle` to get the perpendicular component. Value starts as an estimate. |
| `maximumPossibleForce` | `var maximumPossibleForce: CGFloat { get }` | iOS 9.0 | |
| `altitudeAngle` | `var altitudeAngle: CGFloat { get }` | iOS 9.1 | 0 = parallel to the screen, π/2 = perpendicular. |
| `azimuthAngle(in:)` | `func azimuthAngle(in view: UIView?) -> CGFloat` | iOS 9.1 | `nil` view means window coordinates. 0 rad when the cap end points along +x, increasing clockwise. More expensive than the unit vector. |
| `azimuthUnitVector(in:)` | `func azimuthUnitVector(in view: UIView?) -> CGVector` | iOS 9.1 | Cheaper than the angle. |
| `rollAngle` | `var rollAngle: CGFloat { get }` | **iOS/iPadOS 17.5**, Mac Catalyst 17.5, visionOS 1.2 | 0 for pencils without roll support. |
| `UITouch.Properties.roll` | `static var roll: UITouch.Properties { get }` | 17.5 | Other members: `.force`, `.azimuth`, `.altitude`, `.location`. |
| `estimatedProperties` | `var estimatedProperties: UITouch.Properties { get }` | iOS 9.1 | Values may stay estimated permanently, for example near screen edges. |
| `estimatedPropertiesExpectingUpdates` | `var estimatedPropertiesExpectingUpdates: UITouch.Properties { get }` | iOS 9.1 | An empty set means the value is final. |
| `estimationUpdateIndex` | `var estimationUpdateIndex: NSNumber? { get }` | iOS 9.1 | Increases monotonically. `nil` when nothing is estimated. |
| `UIResponder.touchesEstimatedPropertiesUpdated(_:)` | `func touchesEstimatedPropertiesUpdated(_ touches: Set<UITouch>)` | iOS 9.1 | Also called when an update is no longer expected. |
| `UIGestureRecognizer.touchesEstimatedPropertiesUpdated(_:)` | `func touchesEstimatedPropertiesUpdated(_ touches: Set<UITouch>)` | iOS 9.1 | The default implementation does nothing. |
| `timestamp` | `var timestamp: TimeInterval { get }` | iOS 2.0 | Seconds since system startup (see the time-base section below). |
| `type` / `.pencil` | `var type: UITouch.TouchType { get }` / `case pencil` | 9.0 / 9.1 | `.stylus` is deprecated. Other cases: `.direct`, `.indirect`, `.indirectPointer`. |
| `preciseLocation(in:)` | `func preciseLocation(in view: UIView?) -> CGPoint` | iOS 9.1 | Don't use the result for hit testing. Also `precisePreviousLocation(in:)`. |

URLs follow the pattern https://developer.apple.com/documentation/uikit/uitouch/rollangle, …/uikit/uievent/coalescedtouches(for:), and so on.

**WWDC24 "Squeeze the most out of Apple Pencil" (session 10214):** "Roll is estimated when first sent and later updated over Bluetooth. Be sure to implement touchesEstimatedPropertiesUpdated…". Apple also recommends adding the roll and azimuth angles together, and using direction or velocity as a fallback when roll isn't available.
https://developer.apple.com/videos/play/wwdc2024/10214/

### Time base of `timestamp`
* `UITouch.timestamp` and `UIEvent.timestamp` docs say: "the time, in seconds since system startup … see the `systemUptime` method of `ProcessInfo`".
* `ProcessInfo.systemUptime`: "The amount of time the system has been **awake** since the last time it was restarted". In other words, it doesn't advance during sleep.
* `mach_absolute_time()`: "this clock does not increment while the system is asleep … Prefer … `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)`". `CACurrentMediaTime()` is "derived by calling mach_absolute_time()".
* **Conclusion:** compare touch timestamps against `CACurrentMediaTime()`, `ProcessInfo.processInfo.systemUptime` or `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)`. **Don't use `mach_continuous_time` / CLOCK_MONOTONIC_RAW**, because that clock includes sleep. **[inference from the cited docs, consistent across all of them]**
* **Privacy manifest:** `systemUptime` and `mach_absolute_time` are Required-Reason APIs in the "system boot time" category. The relevant reasons in `NSPrivacyAccessedAPITypeReasons` are:
  * `35F9.1`: measure elapsed time between in-app events. Elapsed times may leave the device.
  * `8FFB.1`: "calculate absolute timestamps for events that occurred within your app, such as events related to the UIKit … frameworks. Absolute timestamps … may be sent off-device. System boot time … may not be sent off-device."
  * **Practical implication:** send session-relative times (t − t₀) or wall-clock-converted times to the Linux PC, not raw uptime values.
  https://developer.apple.com/documentation/bundleresources/app-privacy-configuration/nsprivacyaccessedapitypes/nsprivacyaccessedapitypereasons
* The 17.5 `UIPencilInteraction.Tap.timestamp` and `.Squeeze.timestamp` are also `TimeInterval`. Their time base isn't documented; the uptime base is likely but **unverified**.

### Sample rates
* Apple docs: "Devices that work with Apple Pencil can report touches at up to **240 Hz**. Because UIKit usually reports touches at around 60 Hz, any additional touches are coalesced…" (*Handling input from Apple Pencil*). Also: "You must retrieve coalesced touches immediately when handling an event."
  https://developer.apple.com/documentation/uikit/handling-input-from-apple-pencil ·
  https://developer.apple.com/documentation/uikit/getting-high-fidelity-input-with-coalesced-touches
* **[inference]** UIKit delivers events once per UI update (display refresh):
  * 60 Hz panels (iPad Air M2/M3/M4, iPad mini): about 4 coalesced pencil samples per event.
  * 120 Hz ProMotion (iPad Pro): about 2 per event.
  * Low Power Mode or thermal limits can cap ProMotion at 60 Hz (WWDC21 session 10147).
  * Measure this on device by logging timestamp deltas of the coalesced samples.
* No coalescing exists for hover. `UIHoverGestureRecognizer` updates once per UI update.
* One forum developer logged 19,134 `coalescedTouches(for:)` calls for *finger* touches and every result held a single sample (forum thread 847321). Expect multiple samples only for pencil input.
* **Low-latency option (iOS 18+, needs `if #available` with a 17.5 target):** `UIUpdateLink`.
  * `@MainActor class UIUpdateLink`, `var wantsLowLatencyEventDispatch: Bool`: "Only pencil events are low-latency eligible … dispatch in the middle of a UI update" (gives your handler half the usual time budget).
  * `var wantsImmediatePresentation: Bool`: presents the frame one frame duration sooner.
  * `var requiresContinuousUpdates: Bool`.
  * Initializers: `init(view:)`, `init(windowScene:)` and variants with `actionHandler:`.
  https://developer.apple.com/documentation/uikit/uiupdatelink

### Capture pattern (UIView subclass or a `UIGestureRecognizer` subclass)
Apple sample "Leveraging touch input for drawing apps" (Speed Sketch) uses two recognizers:
* one with `allowedTouchTypes = [UITouch.TouchType.pencil.rawValue as NSNumber]`
* one for `.direct`.

```swift
override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
    for t in touches where t.type == .pencil {
        let samples = event?.coalescedTouches(for: t) ?? [t]
        for s in samples {
            let p = s.preciseLocation(in: self)
            let sample = PencilSample(            // your Sendable struct
                t: s.timestamp, x: p.x, y: p.y,
                force: s.force, maxForce: s.maximumPossibleForce,
                altitude: s.altitudeAngle,
                azimuth: s.azimuthAngle(in: self),
                roll: s.rollAngle,                // 17.5+
                estimated: s.estimatedProperties.rawValue,
                expecting: s.estimatedPropertiesExpectingUpdates.rawValue,
                updateIndex: s.estimationUpdateIndex?.intValue)
            emit(sample)
        }
        for s in event?.predictedTouches(for: t) ?? [] { emitPredicted(s) }  // flag as predicted
    }
}
override func touchesEstimatedPropertiesUpdated(_ touches: Set<UITouch>) {
    for s in touches { if let idx = s.estimationUpdateIndex { emitUpdate(idx.intValue, from: s) } }
}
```
Extract plain values on the main actor. `UITouch` is `@MainActor`, so never hand the object itself to a network queue.

---

## 2. Hover: UIHoverGestureRecognizer

| API | Declaration | Introduced |
|---|---|---|
| class | `class UIHoverGestureRecognizer` (subclass of `@MainActor UIGestureRecognizer`) | iOS 13.0 |
| `zOffset` | `var zOffset: CGFloat { get }` | **16.1** |
| `altitudeAngle` | `var altitudeAngle: CGFloat { get }` | 16.4 |
| `azimuthAngle(in:)` | `func azimuthAngle(in view: UIView?) -> CGFloat` | 16.4 |
| `azimuthUnitVector(in:)` | `func azimuthUnitVector(in view: UIView?) -> CGVector` | 16.4 |
| `rollAngle` | `var rollAngle: CGFloat { get }` | **17.5** |

* **zOffset:** "This value is `1` at the maximum distance from the screen and approaches `0` as the pointing device gets closer … `0` for devices that don't support zOffset." So the range is normalized [0, 1] and you get no physical units. The other properties return 0 (or an empty vector) on devices without support.
* **Not pencil-only by default.** "By default, hover gestures work with pointing devices such as trackpads as well as Apple Pencil." To restrict it: `hover.allowedTouchTypes = [UITouch.TouchType.pencil.rawValue as NSNumber]` (sample "Adopting hover support for Apple Pencil").
  https://developer.apple.com/documentation/uikit/adopting-hover-support-for-apple-pencil
* **Hover height:** Apple's 2022 announcement and the older HIG said "up to 12 mm above the display". The current HIG no longer gives a number **[secondary sources]**.
* **Supported devices** (apple.com/apple-pencil footnote): "Apple Pencil hover works with iPad Pro (M4 and M5), iPad Pro 11-inch (4th generation) … iPad Air (M2, M3, and M4), and iPad mini (A17 Pro and A20 Pro)". So the iPad Air M2/M3 does get Pencil Pro hover.
* Read `prefersHoverToolPreview` to respect the user's hover-preview setting (see §3).
* **Nothing new for hover in iPadOS 26 or 27** (UIKit and Apple Pencil "updates" pages, iOS 26/27 release notes).

---

## 3. UIPencilInteraction (double tap and squeeze)

| API | Declaration | Availability |
|---|---|---|
| class | `@MainActor class UIPencilInteraction` | iOS 12.1 (visionOS 26.2) |
| init | `init(delegate: any UIPencilInteractionDelegate)` | **17.5** |
| delegate | `weak var delegate: (any UIPencilInteractionDelegate)? { get set }` | 12.1 |
| `isEnabled` | `var isEnabled: Bool { get set }` (default `true`) | 12.1 |
| protocol | `@MainActor protocol UIPencilInteractionDelegate : NSObjectProtocol` | 12.1 |
| double tap | `optional func pencilInteraction(_ interaction: UIPencilInteraction, didReceiveTap tap: UIPencilInteraction.Tap)` | **17.5** |
| squeeze | `optional func pencilInteraction(_ interaction: UIPencilInteraction, didReceiveSqueeze squeeze: UIPencilInteraction.Squeeze)` | **17.5** |
| old double tap | `optional func pencilInteractionDidTap(_ interaction: UIPencilInteraction)` | 12.1, **deprecated iOS/iPadOS 17.5**: "Use pencilInteraction(_:didReceiveTap:) instead" |
| `Tap` | `@MainActor class Tap`; `var timestamp: TimeInterval { get }`; `var hoverPose: UIPencilHoverPose? { get }` | 17.5 |
| `Squeeze` | `@MainActor class Squeeze`; `var timestamp: TimeInterval { get }`; `var phase: UIPencilInteraction.Phase { get }`; `var hoverPose: UIPencilHoverPose? { get }` | 17.5 |
| `Phase` | `enum Phase` with cases `.began`, `.changed`, `.ended` ("…or a discrete gesture … recognized") and `.cancelled` | 17.5 |
| `UIPencilHoverPose` | `@MainActor class UIPencilHoverPose` | 17.5 |
| hover pose properties | `var location: CGPoint` ("above the view's bounds, in view's coordinate space"), `var zOffset: CGFloat`, `var altitudeAngle: CGFloat`, `var azimuthAngle: CGFloat`, `var azimuthUnitVector: CGVector`, `var rollAngle: CGFloat` (all `{ get }`). Note: these are **properties, not `(in:)` methods**. | 17.5 |
| `preferredTapAction` | `class var preferredTapAction: UIPencilPreferredAction { get }` | 12.1 |
| `preferredSqueezeAction` | `class var preferredSqueezeAction: UIPencilPreferredAction { get }` | **17.5** |
| `prefersPencilOnlyDrawing` | `class var prefersPencilOnlyDrawing: Bool { get }` | 12.1 |
| `prefersHoverToolPreview` | `class var prefersHoverToolPreview: Bool { get }` | 17.5 |
| `UIPencilPreferredAction` | `.ignore`, `.switchEraser`, `.switchPrevious`, `.showColorPalette`, `.showInkAttributes`, plus 17.5 additions `.showContextualPalette` and `.runSystemShortcut` | 12.1 / 17.5 |

URL root: https://developer.apple.com/documentation/uikit/uipencilinteraction

**Delivery rules**
* `hoverPose` is "`nil` if Apple Pencil isn't close enough to the screen to detect a hover, or if the device doesn't support hover". So **taps and squeezes are delivered without hover**; the pose is simply `nil`.
* HIG: "The squeeze gesture is available only when the paired iPad screen is on and while the Apple Pencil Pro is **not directly contacting** it."
  https://developer.apple.com/design/human-interface-guidelines/apple-pencil-and-scribble
* WWDC24 session 10214 adds three rules:
  * "if the preference is set to run a shortcut, your app will not be sent the squeeze event" (`.runSystemShortcut`).
  * The system shows an undo/redo slider on squeeze in every app. "If you want to suppress this behavior, install a UIPencilInteraction with a delegate that implements the method to respond to squeeze actions."
  * "Treat squeeze as a single gesture that performs a discrete action." Act on `phase == .ended`, but stream every phase.
* Only Apple Pencil Pro supports squeeze (*Handling squeezes from Apple Pencil*):
  https://developer.apple.com/documentation/applepencil/handling-squeezes-from-apple-pencil

---

## 4. Haptics on Apple Pencil Pro

| API | Declaration | Availability |
|---|---|---|
| `UIFeedbackGenerator` | `@MainActor class UIFeedbackGenerator`. Conforms to **`UIInteraction`** and `Sendable`. | iOS 10.0 |
| `init(view:)` | `convenience init(view: UIView)` | **17.5** |
| `init()` | `init()`, **deprecated iOS/iPadOS 27.0** ("Use init(view:) instead") | 10.0 |
| `prepare()` | `func prepare()` | 10.0 |
| `UICanvasFeedbackGenerator` | `@MainActor class UICanvasFeedbackGenerator` | **17.5** (no visionOS) |
| | `func alignmentOccurred(at location: CGPoint)` | 17.5 |
| | `func pathCompleted(at location: CGPoint)` | 17.5 |
| `UIImpactFeedbackGenerator` | `convenience init(style: UIImpactFeedbackGenerator.FeedbackStyle, view: UIView)` (17.5). `init(style:)` is deprecated 27.0. | |
| | `func impactOccurred(at location: CGPoint)` | 17.5 |
| | `func impactOccurred(intensity: CGFloat, at location: CGPoint)` | 17.5 |
| `UISelectionFeedbackGenerator` | `func selectionChanged(at location: CGPoint)` | 17.5 |
| `UINotificationFeedbackGenerator` | `func notificationOccurred(_ notificationType: UINotificationFeedbackGenerator.FeedbackType, at location: CGPoint)` | 17.5 |

**Routing and conditions**
* WWDC24 "What's new in UIKit" (session 10118): "In iPadOS 17.5, this capability [sensory feedback] was expanded to iPad with Apple Pencil Pro and Magic Keyboard." So all `UIFeedbackGenerator` subclasses, not only the canvas one, can play on Pencil Pro.
* WWDC24 session 10214: "UIFeedbackGenerator and all its subclasses have been updated to take a view on initialization and a point in that view … Be sure to update all of your uses."
* *Playing haptic feedback in your app* (https://developer.apple.com/documentation/applepencil/playing-haptic-feedback-in-your-app):
  * The API "doesn't play haptics directly … The system then determines whether to play the haptics".
  * Haptics play only "on a device with hardware for haptic feedback; when the app is running in the foreground; when the system Haptics setting is on".
  * "Don't check the device type or app state … The system ignores any requests that it can't fulfill."
* **No Apple doc states the pencil-proximity condition.**
  * Developer Forums thread 800076 (Sep 2025): the developer reports that only feedback triggered from a pencil-generated touch made the pencil buzz. Finger-initiated feedback didn't, even with the pencil on the glass.
  * The DTS engineer: "I think there was actually some logic in the pencil itself that prevented it from vibrating when it wasn't 'in contact'", and "it's possible we're tying haptics more directly to pen motion."
  * **Working assumption:** trigger from a pencil touch event, at the pencil location, while the pencil is touching. Test hover separately.
  https://developer.apple.com/forums/thread/800076
* The HIG warns against continuous or long haptics on Pencil Pro (https://developer.apple.com/design/human-interface-guidelines/playing-haptics).

---

## 5. App lifecycle, windowing and drawing-surface setup

### Scene life cycle is mandatory with the iOS 27 SDK
* iOS & iPadOS 27 release notes, UIKit deprecations: "Apps built with the latest SDK must adopt the scene-based life cycle or they fail to launch." (141837548)
* *Transitioning to the UIKit scene-based life cycle*: "Beginning in iOS 27, iPadOS 27 … apps built with the latest SDK must adopt the scene-based life cycle or they fail to launch." The article quotes the iOS 26 log text "UIScene lifecycle will soon be required. Failure to adopt will result in an assert in the future." The 18.4 log text was "This process does not adopt UIScene lifecycle. This will become an assert in a future version."
  https://developer.apple.com/documentation/uikit/transitioning-to-the-uikit-scene-based-life-cycle ·
  TN3187: https://developer.apple.com/documentation/technotes/tn3187-migrating-to-the-uikit-scene-based-life-cycle
* What triggers migration: the `UIApplicationSceneManifest` key is missing or has no configurations, **or** `application(_:configurationForConnecting:options:)` isn't implemented. Fix by adding `UIApplicationSceneManifest` (`UIApplicationSupportsMultipleScenes`=false, `UISceneConfigurations` → `UIWindowSceneSessionRoleApplication` → `UISceneDelegateClassName` = `$(PRODUCT_MODULE_NAME).SceneDelegate`), then build the window in `scene(_:willConnectTo:options:)`.
* **Also new with the iOS 27 SDK:** "iOS and iPadOS apps built with the 27.0 SDK or later are required to include a launch screen" (`UILaunchStoryboardName`, `UILaunchStoryboards`, `UILaunchScreen` or `UILaunchScreens`). Otherwise App Store submission is rejected.
* With Xcode 26 / iOS 26 SDK the scene life cycle only logs a warning, but adopt it now.

### UIRequiresFullScreen is deprecated; use resizable scenes
* `UIRequiresFullScreen` is **deprecated in iOS/iPadOS 26.0**. In iPadOS 26 "Windowed Apps" or Stage Manager, the compatibility mode keeps a constant scene size, **scales** the presentation when the user resizes the window, and allows other apps on screen.
* **iOS 27 SDK:** the key no longer opts out of resizing. The system resizes **discretely** instead: the scene moves to a `UIScreen` of the new size when the drag ends (TN3192 and WWDC26 session 278).
* `UIRequiresFullScreenIgnoredStartingWithVersion` (Info.plist string) is available from **iOS 26.2**.
  https://developer.apple.com/documentation/bundleresources/information-property-list/uirequiresfullscreen ·
  https://developer.apple.com/documentation/technotes/tn3192-migrating-your-app-from-the-deprecated-uirequiresfullscreen-key
* Replacement APIs:

  | API | Declaration | Availability | Notes |
  |---|---|---|---|
  | `UIViewController.prefersInterfaceOrientationLocked` | `var prefersInterfaceOrientationLocked: Bool { get }` | **26.0** | Honored only when the scene is centered, screen-sized and not occluded. Call `setNeedsUpdateOfPrefersInterfaceOrientationLocked()` after changing it. |
  | `UIWindowSceneDelegate.windowScene(_:didUpdateEffectiveGeometry:)` | `optional func windowScene(_ windowScene: UIWindowScene, didUpdateEffectiveGeometry previousEffectiveGeometry: UIWindowScene.Geometry)` | 26.0 | |
  | `UIWindowScene.Geometry.isInteractivelyResizing` | `var isInteractivelyResizing: Bool { get }` | 26.0 | |
  | `UIWindowScene.sizeRestrictions` | (`UISceneSizeRestrictions`) | | Sets a preferred minimum size. |
* `UIScreen.main` became formally **deprecated in iOS 26** (iOS 26 release notes). Derive the drawing surface size from the view or window scene, and normalize streamed coordinates by the drawing view's bounds, because those bounds can change at runtime.

### Keeping the screen awake, home indicator, system edge gestures, status bar
* `UIApplication.shared.isIdleTimerDisabled`: `var isIdleTimerDisabled: Bool { get set }` (iOS 2.0). Reset it to `false` when it's no longer needed.
* `UIViewController.prefersHomeIndicatorAutoHidden`: `var prefersHomeIndicatorAutoHidden: Bool { get }` (11.0). This is only a preference ("no guarantee"). Pair it with `setNeedsUpdateOfHomeIndicatorAutoHidden()`.
* `UIViewController.preferredScreenEdgesDeferringSystemGestures`: `var preferredScreenEdgesDeferringSystemGestures: UIRectEdge { get }` (11.0). Pair it with `setNeedsUpdateOfScreenEdgesDeferringSystemGestures()`. For container VCs use `childForScreenEdgesDeferringSystemGestures` / `childForHomeIndicatorAutoHidden`.
* `prefersStatusBarHidden` + `setNeedsStatusBarAppearanceUpdate()`.
* **[unverified]** Whether deferring edges also suppresses the Pencil corner-swipe gestures (screenshot / Quick Note): test on device.
* If video is shown, `AVSampleBufferDisplayLayer.preventsDisplaySleepDuringVideoPlayback` (`var … : Bool { get set }`, iOS 13) also keeps the display awake while frames are enqueued.

### Palm rejection, multi-touch, pencil-only behavior
* **No public palm-rejection API exists.** DTS on forum thread 847321: "Palm rejection has no public API. The word does not occur in any framework header in the iOS 27 SDK … nothing on `UITouch` reports that a contact was rejected."
  * Your code must handle `touchesCancelled`.
  * DTS recommends separate recognizers filtered by `allowedTouchTypes` (`.pencil` vs `.direct`), both with `cancelsTouchesInView = false`.
  * Set `requiresExclusiveTouchType` deliberately: `var requiresExclusiveTouchType: Bool` (iOS 9.2). When `true` (the default), the recognizer "ignores new touches whose type doesn't match the type of the initial touch".
  https://developer.apple.com/forums/thread/847321
* `UIGestureRecognizer.allowedTouchTypes`: `var allowedTouchTypes: [NSNumber] { get set }` (9.0).
* `UIView.isMultipleTouchEnabled`: `var isMultipleTouchEnabled: Bool { get set }`. Default `false`. It doesn't affect gesture recognizers.
* `UIView.isExclusiveTouch`.
* Respect `UIPencilInteraction.prefersPencilOnlyDrawing` (the system "draw with Apple Pencil only" setting): fingers pan, the pencil draws (the Speed Sketch pattern).

---

## 6. Video display: AVSampleBufferDisplayLayer and the decode path

| API | Declaration | Availability |
|---|---|---|
| `AVSampleBufferDisplayLayer` | `@MainActor class AVSampleBufferDisplayLayer` (conforms to `Sendable`) | iOS 8.0 |
| `sampleBufferRenderer` | `var sampleBufferRenderer: AVSampleBufferVideoRenderer { get }`. Docs: "allows you to safely enqueue sample buffers from a background thread." | **iOS 17.0** |
| `layer.enqueue(_:)` | `func enqueue(_ sampleBuffer: CMSampleBuffer)`. **Deprecated iOS 18.0**: "Use sampleBufferRenderer's enqueueSampleBuffer: instead". `flush()`, `flushAndRemoveImage()`, `status`, `error`, `isReadyForMoreMediaData`, `requestMediaDataWhenReady`, `requiresFlushToResumeDecoding` and `timebase` on the layer are deprecated too. `controlTimebase` and `videoGravity` remain. | 8.0 to 18.0 |
| `AVSampleBufferVideoRenderer` | `class AVSampleBufferVideoRenderer`. **Not `Sendable`.** | 17.0 |
| `renderer.enqueue(_:)` | `func enqueue(_ sampleBuffer: CMSampleBuffer)`. **Deprecated iOS 27.0**: "Attach renderer to a render synchronizer with sampleBufferReceiver(adding:) and use the receiver's enqueue(_:) async or enqueueImmediately(_:) methods instead". `flush()`, `flush(removingDisplayedImage:completionHandler:)`, `status`, `error`, `isReadyForMoreMediaData`, `requiresFlushToResumeDecoding` and the decode-failure notifications are also deprecated in 27.0. | 11.0 to 27.0 |
| `presentationTimeExpectation` | `var presentationTimeExpectation: AVSampleBufferVideoRenderer.PresentationTimeExpectation { get set }` with cases `.none`, `.monotonicallyIncreasing`, `.minimumUpcoming(CMTime)`. Undocumented. | 17.4 |
| **New in 27:** `AVSampleBufferRenderSynchronizer.sampleBufferReceiver(adding:)` | `func sampleBufferReceiver(adding renderer: AVSampleBufferVideoRenderer) -> sending AVSampleBufferVideoRenderer.Receiver` | **27.0** |
| `Receiver.enqueue(_:)` | `nonisolated(nonsending) func enqueue(_ sampleBuffer: CMReadySampleBuffer<CMSampleBuffer.DynamicContent>) async throws -> AVSampleBufferVideoRenderer.Receiver.EnqueueResult` | 27.0 |
| `Receiver.enqueueImmediately(_:)` | `func enqueueImmediately(_ sampleBuffer: CMReadySampleBuffer<CMSampleBuffer.DynamicContent>) -> …EnqueueResult` | 27.0 |
| `EnqueueResult` | `.enqueued`, `.enqueuedWithDecodeFailures(_)`, `.cancelledDueToFlush`, `.cancelledDueToFlushRequiredToResume(_)`, `.cancelledDueToError(_)` | 27.0 |
| other `Receiver` members | `flush()`, `flush(removingDisplayedImage:)` (async), `renderingEventsAfterFinishedEnqueuing: some Sendable & AsyncSequence<RenderingEvent, Never>` | 27.0 |
| `CMReadySampleBuffer` | `struct CMReadySampleBuffer<Content>`, e.g. `init(unsafeBuffer:)` to wrap an existing `CMSampleBuffer` | 26.0 |

URLs: https://developer.apple.com/documentation/avfoundation/avsamplebufferdisplaylayer ·
…/avfoundation/avsamplebuffervideorenderer · …/avfoundation/avsamplebuffervideorenderer/receiver

**Recommendation for a 17.5 deployment target**
* Use `layer.sampleBufferRenderer.enqueue(_:)` on iOS 17.5 through 26.
* Optionally add an `if #available(iOS 27, *)` path with a synchronizer and receiver. **[inference]** Swift compares deprecations against the deployment target, so a 17.5 target shouldn't produce warnings for the 18.0 or 27.0 deprecations.
* Never call `layer.enqueue` (deprecated 18.0).

**Building CMSampleBuffers from Annex-B NAL units**
* `func CMVideoFormatDescriptionCreateFromH264ParameterSets(allocator: CFAllocator?, parameterSetCount: Int, parameterSetPointers: UnsafePointer<UnsafePointer<UInt8>>, parameterSetSizes: UnsafePointer<Int>, nalUnitHeaderLength NALUnitHeaderLength: Int32, formatDescriptionOut: UnsafeMutablePointer<CMFormatDescription?>) -> OSStatus` (iOS 7.0)
  * SPS (7) + PPS (8), optional SPS-ext (13), at least 2 sets.
  * Pass raw NALs **without start codes**, keeping emulation-prevention bytes.
  * `nalUnitHeaderLength` = 1, 2 or 4. Use 4.
* `func CMVideoFormatDescriptionCreateFromHEVCParameterSets(allocator: CFAllocator?, parameterSetCount: Int, parameterSetPointers: UnsafePointer<UnsafePointer<UInt8>>, parameterSetSizes: UnsafePointer<Int>, nalUnitHeaderLength NALUnitHeaderLength: Int32, extensions: CFDictionary?, formatDescriptionOut: UnsafeMutablePointer<CMFormatDescription?>) -> OSStatus` (iOS 11.0)
  * Apple's text wrongly copies the H.264 NAL types. In practice pass VPS (32), SPS (33) and PPS (34). **[practice, not in the doc]**
* `func CMBlockBufferCreateWithMemoryBlock(allocator: CFAllocator?, memoryBlock: UnsafeMutableRawPointer?, blockLength: Int, blockAllocator: CFAllocator?, customBlockSource: UnsafePointer<CMBlockBufferCustomBlockSource>?, offsetToData: Int, dataLength: Int, flags: CMBlockBufferFlags, blockBufferOut: UnsafeMutablePointer<CMBlockBuffer?>) -> OSStatus`
* `func CMBlockBufferReplaceDataBytes(with sourceBytes: UnsafeRawPointer, blockBuffer destinationBuffer: CMBlockBuffer, offsetIntoDestination: Int, dataLength: Int) -> OSStatus`
* `func CMSampleBufferCreateReady(allocator: CFAllocator?, dataBuffer: CMBlockBuffer?, formatDescription: CMFormatDescription?, sampleCount: CMItemCount, sampleTimingEntryCount: CMItemCount, sampleTimingArray: UnsafePointer<CMSampleTimingInfo>?, sampleSizeEntryCount: CMItemCount, sampleSizeArray: UnsafePointer<Int>?, sampleBufferOut: UnsafeMutablePointer<CMSampleBuffer?>) -> OSStatus`
* `kCMSampleAttachmentKey_DisplayImmediately: CFString`: "the sample should be displayed as soon as possible rather than according to its presentation timestamp … from a display pipeline such as AVSampleBufferDisplayLayer."
  * Swift overlay (iOS 13+): `sampleBuffer.sampleAttachments[0][.displayImmediately] = true`.
  * C route: `CMSampleBufferGetSampleAttachmentsArray(_:createIfNecessary: true)`, then set the key in the first dictionary.

```swift
// Annex-B access unit -> AVCC: replace each 00 00 01 / 00 00 00 01 start code with a 4-byte big-endian length.
// Drop AUD/SPS/PPS (and VPS) NALs from the sample payload. Rebuild the format description when SPS/PPS change.
func makeFormat(sps: [UInt8], pps: [UInt8]) -> CMVideoFormatDescription? {
    var fd: CMFormatDescription?
    let st = sps.withUnsafeBufferPointer { s in pps.withUnsafeBufferPointer { p in
        CMVideoFormatDescriptionCreateFromH264ParameterSets(
            allocator: kCFAllocatorDefault, parameterSetCount: 2,
            parameterSetPointers: [s.baseAddress!, p.baseAddress!],
            parameterSetSizes: [sps.count, pps.count],
            nalUnitHeaderLength: 4, formatDescriptionOut: &fd) } }
    return st == noErr ? fd : nil
}
func makeSample(avcc: Data, fmt: CMVideoFormatDescription) -> CMSampleBuffer? {
    var bb: CMBlockBuffer?
    guard CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil,
            blockLength: avcc.count, blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
            offsetToData: 0, dataLength: avcc.count, flags: kCMBlockBufferAssureMemoryNowFlag,
            blockBufferOut: &bb) == kCMBlockBufferNoErr, let bb else { return nil }
    _ = avcc.withUnsafeBytes { CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: bb,
                                   offsetIntoDestination: 0, dataLength: avcc.count) }
    var sb: CMSampleBuffer?; var size = avcc.count
    guard CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: bb, formatDescription: fmt,
            sampleCount: 1, sampleTimingEntryCount: 0, sampleTimingArray: nil,
            sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sb) == noErr,
          let sb else { return nil }
    sb.sampleAttachments[0][.displayImmediately] = true
    return sb
}
```

**VTDecompressionSession alternative** (decode yourself, then display through Metal / `CAMetalLayer`, or enqueue `CVPixelBuffer`-backed samples):
* `func VTDecompressionSessionCreate(allocator: CFAllocator?, formatDescription: CMVideoFormatDescription, decoderSpecification: CFDictionary?, imageBufferAttributes: CFDictionary?, decompressionSessionOut: UnsafeMutablePointer<VTDecompressionSession?>) -> OSStatus` (**iOS 17.0**, no callback record)
* `func VTDecompressionSessionDecodeFrame(_ session: VTDecompressionSession, sampleBuffer: CMSampleBuffer, flags: VTDecodeFrameFlags, infoFlagsOut: UnsafeMutablePointer<VTDecodeInfoFlags>?, completionHandler: @escaping @Sendable (OSStatus, VTDecodeInfoFlags, CVImageBuffer?, [CMTaggedBuffer]?, CMTime, CMTime) -> Void) -> OSStatus` (17.0). Can't be used with a session created with a callback record.
* Older form: `VTDecompressionSessionDecodeFrame(_:sampleBuffer:flags:infoFlagsOut:outputHandler:)` (iOS 9.0) with `typealias VTDecompressionOutputHandler = @Sendable (OSStatus, VTDecodeInfoFlags, CVImageBuffer?, CMTime, CMTime) -> Void`.
* `kVTDecompressionPropertyKey_RealTime: CFString`.
* Leave `kVTDecodeFrame_EnableTemporalProcessing` clear. Async decompression is optional.

**Lowest-latency recommendations** **[practice / inference]**
* Encode on Linux without B-frames (baseline/main, `bframes=0`), IDR or intra-refresh, one slice or a whole access unit per message.
* Set DisplayImmediately on every sample. Alternatively, drive `controlTimebase` and set `presentationTimeExpectation = .monotonicallyIncreasing` (undocumented semantics: measure it).
* Enqueue from one serial background queue.
* Flush and wait for an IDR after `requiresFlushToResumeDecoding` (pre-27) or `.cancelledDueToFlushRequiredToResume` (27+).
* Consider `AVSampleBufferDisplayLayerFailedToDecode` notifications for error handling.
* `kVTDecompressionPropertyKey_RealTime = true` if you use VT directly.
* TCP_NODELAY (`NWProtocolTCP.Options.noDelay`, iOS 12) on the video and input sockets.

**Swift 6 gotchas (video)**
* `CMSampleBuffer` and `VTDecompressionSession` are **not `Sendable`**. `CMFormatDescription` *is* `Sendable`.
* `AVSampleBufferVideoRenderer` is not `Sendable`, even though the docs call background enqueue safe. Read `layer.sampleBufferRenderer` once on the main actor and keep it in a `nonisolated(unsafe)` property or an `@unchecked Sendable` box owned by your decode queue. Use `sending` parameters for sample buffers.

---

## 7. Network.framework (listener, Bonjour, TLS, framing)

| API | Declaration | Availability |
|---|---|---|
| `NWListener` | `final class NWListener` (`Sendable`) | 12.0 |
| init | `init(using: NWParameters, on: NWEndpoint.Port = .any) throws` | 12.0 |
| `newConnectionHandler` | `@preconcurrency final var newConnectionHandler: (@Sendable (NWConnection) -> Void)? { get set }` | 12.0 |
| `stateUpdateHandler` | `@preconcurrency final var stateUpdateHandler: (@Sendable (NWListener.State) -> Void)? { get set }` | 12.0 |
| `service` | `final var service: NWListener.Service? { get set }` | 12.0 |
| `serviceRegistrationUpdateHandler` | `@preconcurrency final var serviceRegistrationUpdateHandler: (@Sendable (NWListener.ServiceRegistrationChange) -> Void)?` | 12.0 |
| `NWListener.Service` | `init(name: String? = nil, type: String, domain: String? = nil, txtRecord: Data? = nil)` (12.0); `init(name: String? = nil, type: String, domain: String? = nil, txtRecord: NWTXTRecord)` (**13.0**); `noAutoRename` | |
| `NWTXTRecord` | `struct NWTXTRecord`; `init(_ dictionary: [String : String] = [:])`; `subscript`, `setEntry(_:for:)`, `dataDictionary` | 13.0 |
| `NWParameters.requiredInterfaceType` | `final var requiredInterfaceType: NWInterface.InterfaceType { get set }`; `.loopback` | 12.0 |
| other `NWParameters` | `requiredLocalEndpoint: NWEndpoint?`, `acceptLocalOnly: Bool` | 12.0 |
| `NWParameters(tls:tcp:)` | `convenience init(tls: NWProtocolTLS.Options?, tcp: NWProtocolTCP.Options = NWProtocolTCP.Options())` | 12.0 |
| `NWProtocolTLS.Options.securityProtocolOptions` | `var securityProtocolOptions: sec_protocol_options_t { get }` | 12.0 |
| `sec_identity_create` | `func sec_identity_create(_ identity: SecIdentity) -> sec_identity_t?` | 12.0 |
| `sec_protocol_options_set_local_identity` | `func sec_protocol_options_set_local_identity(_ options: sec_protocol_options_t, _ identity: sec_identity_t)` | 12.0 |
| `sec_protocol_options_set_peer_authentication_required` | `func sec_protocol_options_set_peer_authentication_required(_ options: sec_protocol_options_t, _ peer_authentication_required: Bool)`. "Clients default to true, whereas servers default to false." | 12.0 |
| `sec_protocol_options_set_verify_block` | `func sec_protocol_options_set_verify_block(_ options: sec_protocol_options_t, _ verify_block: @escaping sec_protocol_verify_t, _ verify_block_queue: dispatch_queue_t)` | 12.0 |
| `sec_protocol_verify_t` | `typealias sec_protocol_verify_t = (sec_protocol_metadata_t, sec_trust_t, @escaping sec_protocol_verify_complete_t) -> Void`. **Not `@Sendable`.** "may be called one or more times for a given connection." | |
| helpers | `sec_trust_copy_ref(_:) -> Unmanaged<SecTrust>`; `sec_protocol_metadata_access_peer_certificate_chain(_:_:)`; `sec_protocol_options_set_min_tls_protocol_version(_:_:)` (13.0) | |
| `NWConnection.receive` | `@preconcurrency final func receive(minimumIncompleteLength: Int, maximumLength: Int, completion: @escaping @Sendable (Data?, NWConnection.ContentContext?, Bool, NWError?) -> Void)` | 12.0 |
| `NWConnection.receiveMessage` | `receiveMessage(completion:)`. On TCP/TLS this delivers nothing until the stream closes, unless a framer is in the stack. | 12.0 |
| `NWConnection.send` | `@preconcurrency final func send(content: Data?, contentContext: NWConnection.ContentContext = .defaultMessage, isComplete: Bool = true, completion: NWConnection.SendCompletion)`; generic `DataProtocol` variant (13.0) | 12.0 |
| `NWConnection.start(queue:)` | `final func start(queue: DispatchQueue)` | 12.0 |
| `NWConnection.stateUpdateHandler` | `@Sendable` | 12.0 |
| `NWProtocolFramer` | `NWProtocolFramer.Definition(implementation: any NWProtocolFramerImplementation.Type)`, `NWProtocolFramer.Options(definition:)`. `Instance` has `parseInput(minimumIncompleteLength:maximumLength:parse:)`, `deliverInputNoCopy(length:message:isComplete:)` and `writeOutput(data:)`. | 13.0 |

**Notes**
* **Loopback plus Bonjour don't mix.** `requiredInterfaceType = .loopback` binds the listener to loopback, which suits a USB usbmuxd/iproxy tunnel because forwarded connections arrive on the device's localhost **[inference]**. A Bonjour advertisement on that same listener can only be seen on loopback, so the Linux PC won't find it over Wi-Fi or Ethernet. Use two listeners (loopback for USB, a normal one with `service` for the LAN), or skip Bonjour for USB.
* **Local network privacy (TN3179):** "Listening for and accepting incoming TCP connections" does **not** need local-network access. "Registering a service with Bonjour" **does**. That requires `NSLocalNetworkUsageDescription` plus `NSBonjourServices` (e.g. `_myapp._tcp`) and triggers the user prompt.
  https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy
* **TLS:**
  * Since iOS 26 the default minimum TLS version for URLSession and Network.framework is 1.2 (iOS 26 release notes).
  * iOS 27 enforces stricter TLS only for system processes (MDM, app install, …), not third-party apps.
  * Apple's article *Creating an Identity for Local Network TLS* shows the listener-side pattern: `SecPKCS12Import`, keychain, `sec_identity_create`, `sec_protocol_options_set_local_identity`, min TLSv12.
  * For mutual TLS / pinning use `sec_protocol_options_set_peer_authentication_required(opts, true)` plus a verify block that pins the Linux client's certificate.
  https://developer.apple.com/documentation/network/creating-an-identity-for-local-network-tls
* **Length framing without a framer:** call `receive(minimumIncompleteLength: 4, maximumLength: 4)` for the header, then `receive(minimumIncompleteLength: n, maximumLength: n)` for the body, re-arming after each completion. `NWProtocolFramer` is optional. Use it if you want `receiveMessage` semantics.
* **New Swift-native APIs (iOS 26.0, need `if #available` with a 17.5 target):** `final class NetworkListener<ApplicationProtocol>`, `NetworkConnection`, a built-in `TLV` framer, `Coder`/`NetworkJSONCoder`, `BonjourListenerProvider` (structured concurrency, WWDC25 "Use structured concurrency with Network framework"). Optional for this project.

**Swift 6 strict concurrency gotchas (Network / Security)**
* All NW handlers are `@Sendable` (`@preconcurrency`), and NW types are `Sendable`. A closure formed inside a `@MainActor` type can't touch main-actor state synchronously; hop with `Task { @MainActor in … }`. If you `start(queue: .main)`, use `MainActor.assumeIsolated { … }`.
* `sec_protocol_verify_t` is **not** `@Sendable`. **[inference from SE-0423 dynamic isolation]** A verify closure written inside a `@MainActor` method is inferred main-actor-isolated, and Swift 6 inserts a runtime isolation check. That check traps when the closure runs on your TLS queue. Build TLS options in a `nonisolated` function, or annotate the closure `{ @Sendable meta, trust, complete in … }`.
* Don't capture `UITouch`, `UIPencilInteraction.Squeeze` or `UIPencilHoverPose` objects in network closures. They're `@MainActor`. Copy them into `Sendable` structs first.
* `UIPencilInteractionDelegate` and `UIFeedbackGenerator` are `@MainActor`. Conform from a `@MainActor` view controller.

---

## Sources (main)
* UIKit doc JSON, all symbol pages above: https://developer.apple.com/documentation/uikit/uitouch , /uievent , /uihovergesturerecognizer , /uipencilinteraction , /uipencilhoverpose , /uicanvasfeedbackgenerator , /uifeedbackgenerator , /uiimpactfeedbackgenerator , /uiupdatelink , /uiviewcontroller/preferredscreenedgesdeferringsystemgestures , /uiapplication/isidletimerdisabled
* Articles and samples: /uikit/handling-input-from-apple-pencil , /uikit/getting-high-fidelity-input-with-coalesced-touches , /uikit/adopting-hover-support-for-apple-pencil , /uikit/leveraging-touch-input-for-drawing-apps , /applepencil/handling-squeezes-from-apple-pencil , /applepencil/handling-double-taps-from-apple-pencil , /applepencil/playing-haptic-feedback-in-your-app , /updates/applepencil , /updates/uikit
* HIG: /design/human-interface-guidelines/apple-pencil-and-scribble , /playing-haptics
* WWDC: 2024/10214 (Squeeze the most out of Apple Pencil), 2024/10118 (What's new in UIKit), 2025/282 (Make your UIKit app more flexible), 2026/278 (Modernize your UIKit app), 2026/203 (PencilKit), 2021/10147 (variable refresh rate)
* Release notes: /ios-ipados-release-notes/ios-ipados-27-release-notes , …/ios-ipados-26-release-notes , /xcode-release-notes/xcode-27-release-notes ; https://developer.apple.com/news/upcoming-requirements/
* Tech notes: TN3187, TN3192, TN3179 ; /uikit/transitioning-to-the-uikit-scene-based-life-cycle
* AVFoundation, CoreMedia, VideoToolbox: /avfoundation/avsamplebufferdisplaylayer , /avfoundation/avsamplebuffervideorenderer(/receiver) , /avfoundation/avsamplebufferrendersynchronizer , /coremedia/cmvideoformatdescriptioncreatefromh264parametersets(…) , /coremedia/kcmsampleattachmentkey_displayimmediately , /videotoolbox/vtdecompressionsession-api-collection
* Network and Security: /network/nwlistener , /network/nwconnection , /network/nwprotocolframer , /network/networklistener , /network/creating-an-identity-for-local-network-tls , /security/sec_protocol_options_set_verify_block(_:_:_:)
* Forums: https://developer.apple.com/forums/thread/800076 (Pencil Pro haptics), https://developer.apple.com/forums/thread/847321 (palm rejection)
* Apple hardware pages: https://support.apple.com/en-us/108937 , https://www.apple.com/apple-pencil/
