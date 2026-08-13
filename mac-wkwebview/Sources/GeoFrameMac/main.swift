import AppKit

let application = NSApplication.shared
let delegate = AppDelegate(configuration: .load())

application.setActivationPolicy(.regular)
application.delegate = delegate
application.run()
