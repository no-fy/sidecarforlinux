#if canImport(UIKit)
import SidecarNet
import UIKit

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        _ = SessionClock.epoch  // pin the session clock origin at launch
        return true
    }

    func application(_ application: UIApplication,
                     configurationForConnecting connectingSceneSession: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let config = UISceneConfiguration(name: "Default Configuration", sessionRole: connectingSceneSession.role)
        config.delegateClass = SceneDelegate.self  // in addition to the Info.plist entry
        return config
    }
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private var controller: AppController?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession,
               options connectionOptions: UIScene.ConnectionOptions) {
        guard let windowScene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: windowScene)
        let root = RootViewController()
        window.rootViewController = root
        window.makeKeyAndVisible()
        self.window = window
        let controller = AppController(root: root)
        root.controller = controller
        self.controller = controller
        controller.start()
    }

    func sceneDidBecomeActive(_ scene: UIScene) {
        controller?.sceneBecameActive()
    }

    func sceneWillResignActive(_ scene: UIScene) {
        controller?.sceneWillResignActive()
    }
}
#endif
