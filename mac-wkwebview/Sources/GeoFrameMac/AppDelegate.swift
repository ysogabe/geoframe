import AppKit
import WebKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate, WKNavigationDelegate {
    private let configuration: AppConfiguration
    private var window: NSWindow?
    private var webView: WKWebView?
    private var statusLabel: NSTextField?
    private var retryTimer: Timer?

    init(configuration: AppConfiguration) {
        self.configuration = configuration
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        installMainMenu()
        createWindow()
        loadHomePage()
    }

    func applicationWillTerminate(_ notification: Notification) {
        retryTimer?.invalidate()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    private func createWindow() {
        let webConfiguration = WKWebViewConfiguration()
        webConfiguration.websiteDataStore = .default()
        webConfiguration.defaultWebpagePreferences.allowsContentJavaScript = true
        webConfiguration.defaultWebpagePreferences.preferredContentMode = .desktop
        webConfiguration.mediaTypesRequiringUserActionForPlayback = []

        let webView = WKWebView(frame: .zero, configuration: webConfiguration)
        webView.translatesAutoresizingMaskIntoConstraints = false
        webView.navigationDelegate = self
        webView.allowsMagnification = false
        if #available(macOS 13.3, *) {
            webView.isInspectable = configuration.developerMode
        }

        let statusLabel = NSTextField(labelWithString: waitingMessage)
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        statusLabel.alignment = .center
        statusLabel.font = .systemFont(ofSize: 17, weight: .medium)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.maximumNumberOfLines = 3

        let container = NSView()
        container.wantsLayer = true
        container.layer?.backgroundColor = NSColor.black.cgColor
        container.addSubview(webView)
        container.addSubview(statusLabel)

        NSLayoutConstraint.activate([
            webView.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            webView.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            webView.topAnchor.constraint(equalTo: container.topAnchor),
            webView.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            statusLabel.centerXAnchor.constraint(equalTo: container.centerXAnchor),
            statusLabel.centerYAnchor.constraint(equalTo: container.centerYAnchor),
            statusLabel.leadingAnchor.constraint(greaterThanOrEqualTo: container.leadingAnchor, constant: 32),
            statusLabel.trailingAnchor.constraint(lessThanOrEqualTo: container.trailingAnchor, constant: -32),
        ])

        let initialFrame = NSRect(x: 0, y: 0, width: 1280, height: 720)
        let window = NSWindow(
            contentRect: initialFrame,
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "GeoFrame"
        window.contentView = container
        window.delegate = self
        window.setFrameAutosaveName("GeoFrameMainWindow")
        window.center()
        window.makeKeyAndOrderFront(nil)

        self.window = window
        self.webView = webView
        self.statusLabel = statusLabel

        NSApp.activate(ignoringOtherApps: true)
        if configuration.startsFullscreen {
            DispatchQueue.main.async { [weak self] in
                self?.window?.toggleFullScreen(nil)
            }
        }
    }

    private var waitingMessage: String {
        "GeoFrame serverを待っています…\n\(configuration.homeURL.absoluteString)"
    }

    private func loadHomePage() {
        retryTimer?.invalidate()
        retryTimer = nil
        statusLabel?.stringValue = waitingMessage
        statusLabel?.isHidden = false

        let request = URLRequest(
            url: configuration.homeURL,
            cachePolicy: .reloadRevalidatingCacheData,
            timeoutInterval: 10
        )
        webView?.load(request)
    }

    private func scheduleRetry(after seconds: TimeInterval = 1) {
        retryTimer?.invalidate()
        retryTimer = Timer.scheduledTimer(
            timeInterval: seconds,
            target: self,
            selector: #selector(retryLoad),
            userInfo: nil,
            repeats: false
        )
    }

    @objc private func retryLoad() {
        loadHomePage()
    }

    @objc private func reloadPage() {
        loadHomePage()
    }

    @objc private func toggleFullscreen() {
        window?.toggleFullScreen(nil)
    }

    private func installMainMenu() {
        let mainMenu = NSMenu()

        let applicationItem = NSMenuItem()
        let applicationMenu = NSMenu()
        applicationMenu.addItem(
            withTitle: "GeoFrameについて",
            action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)),
            keyEquivalent: ""
        )
        applicationMenu.addItem(.separator())
        applicationMenu.addItem(
            withTitle: "GeoFrameを終了",
            action: #selector(NSApplication.terminate(_:)),
            keyEquivalent: "q"
        )
        applicationItem.submenu = applicationMenu
        mainMenu.addItem(applicationItem)

        let viewItem = NSMenuItem()
        let viewMenu = NSMenu(title: "表示")
        let reloadItem = NSMenuItem(title: "再読み込み", action: #selector(reloadPage), keyEquivalent: "r")
        reloadItem.target = self
        viewMenu.addItem(reloadItem)

        let fullscreenItem = NSMenuItem(
            title: "フルスクリーン切替",
            action: #selector(toggleFullscreen),
            keyEquivalent: "f"
        )
        fullscreenItem.keyEquivalentModifierMask = [.command, .control]
        fullscreenItem.target = self
        viewMenu.addItem(fullscreenItem)
        viewItem.submenu = viewMenu
        mainMenu.addItem(viewItem)

        NSApp.mainMenu = mainMenu
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        retryTimer?.invalidate()
        retryTimer = nil
        statusLabel?.isHidden = true
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        handleLoadFailure(error)
    }

    func webView(
        _ webView: WKWebView,
        didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        handleLoadFailure(error)
    }

    private func handleLoadFailure(_ error: Error) {
        statusLabel?.stringValue = "\(waitingMessage)\n\(error.localizedDescription)"
        statusLabel?.isHidden = false
        scheduleRetry()
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        statusLabel?.stringValue = "Web表示を再起動しています…"
        statusLabel?.isHidden = false
        scheduleRetry(after: 0.5)
    }

    func windowDidEnterFullScreen(_ notification: Notification) {
        if configuration.hidesCursor {
            NSCursor.setHiddenUntilMouseMoves(true)
        }
    }
}
