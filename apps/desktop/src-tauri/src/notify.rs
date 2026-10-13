//! Native notifications through UNUserNotificationCenter: macOS asks for permission on the
//! first one (and the answer in System Settings → Notifications is respected from then on), and
//! a click opens that chat. Silent (no sound). Only inside an `.app` bundle: the framework
//! aborts in a bare `cargo run` binary, which gets no notifications.

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::OnceLock;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AnyThread, define_class, msg_send};
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions,
        UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };

    type OnClick = Box<dyn Fn(String, String) + Send + Sync>;
    static ON_CLICK: OnceLock<OnClick> = OnceLock::new();
    static AVAILABLE: OnceLock<bool> = OnceLock::new();

    /// Request ids: `falos|<project>|<chat>|<n>`.
    fn parse(id: &str) -> Option<(String, String)> {
        let mut p = id.split('|');
        (p.next()? == "falos").then_some(())?;
        Some((p.next()?.to_string(), p.next()?.to_string()))
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "FalosNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            /// Also show banners while the app is frontmost (its window hidden or in the back).
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _n: &UNNotification,
                done: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                done.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                done: &block2::DynBlock<dyn Fn()>,
            ) {
                let id = response.notification().request().identifier().to_string();
                if let (Some((project, chat)), Some(f)) = (parse(&id), ON_CLICK.get()) {
                    f(project, chat);
                }
                done.call(());
            }
        }
    );

    fn available() -> bool {
        *AVAILABLE.get_or_init(|| {
            let b = NSBundle::mainBundle();
            b.bundleIdentifier().is_some() && b.bundlePath().to_string().ends_with(".app")
        })
    }

    pub fn init(on_click: impl Fn(String, String) + Send + Sync + 'static) {
        if !available() || ON_CLICK.set(Box::new(on_click)).is_err() {
            return;
        }
        let d: Retained<Delegate> = unsafe { msg_send![super(Delegate::alloc().set_ivars(())), init] };
        UNUserNotificationCenter::currentNotificationCenter().setDelegate(Some(ProtocolObject::from_ref(&*d)));
        // The center holds its delegate weakly: keep ours for the app's lifetime.
        std::mem::forget(d);
    }

    pub fn post(project: &str, chat: &str, title: &str, body: &str) {
        if !available() {
            return;
        }
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        let id = NSString::from_str(&format!("falos|{project}|{chat}|{n}"));
        let req = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
        // Asks once; afterwards answers from the user's setting without a prompt.
        let ask = RcBlock::new(move |granted: Bool, _e: *mut NSError| {
            if granted.as_bool() {
                UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&req, None);
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &ask);
    }

    #[cfg(test)]
    #[test]
    fn ids() {
        assert_eq!(parse("falos|p|c_1|3"), Some(("p".into(), "c_1".into())));
        assert_eq!(parse("other|p|c|1"), None);
    }
}

#[cfg(target_os = "macos")]
pub use mac::{init, post};

#[cfg(not(target_os = "macos"))]
pub fn init(_on_click: impl Fn(String, String) + Send + Sync + 'static) {}
#[cfg(not(target_os = "macos"))]
pub fn post(_project: &str, _chat: &str, _title: &str, _body: &str) {}
