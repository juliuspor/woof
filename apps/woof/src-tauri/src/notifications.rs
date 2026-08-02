//! Privacy-preserving local notification delivery on macOS.

use reqwest::Method;
use serde_json::{json, Value};
use tauri::Manager;

const MAX_NUDGE_ID_BYTES: usize = 36;
const MAX_TITLE_BYTES: usize = 256;
const MAX_BODY_BYTES: usize = 2_048;
const MAX_DEEP_LINK_BYTES: usize = 4_096;
const SYSTEM_NOTIFICATION_TITLE: &str = "woof reminder";
const SYSTEM_NOTIFICATION_BODY: &str = "Open woof to view it.";

#[derive(Clone, Debug)]
struct ValidatedNudge {
    id: String,
    title: String,
    body: String,
    deep_link: Option<String>,
    delivered_at: Option<i64>,
}

impl ValidatedNudge {
    fn from_json(value: &Value) -> Option<Self> {
        let id = value.get("nudge_id")?.as_str()?;
        let title = value.get("title")?.as_str()?.trim();
        let body = value.get("body")?.as_str()?.trim();
        if !valid_nudge_id(id)
            || !valid_text(title, MAX_TITLE_BYTES)
            || !valid_text(body, MAX_BODY_BYTES)
        {
            return None;
        }

        let deep_link = match value.get("deep_link") {
            None | Some(Value::Null) => None,
            Some(Value::String(link)) if valid_deep_link(link) => Some(link.clone()),
            Some(Value::String(_)) => None,
            Some(_) => return None,
        };
        let delivered_at = match value.get("sent_at") {
            None | Some(Value::Null) => None,
            Some(Value::Number(timestamp)) => timestamp.as_i64().filter(|value| *value >= 0),
            Some(_) => return None,
        };

        Some(Self {
            id: id.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            deep_link,
            delivered_at,
        })
    }
}

fn valid_nudge_id(value: &str) -> bool {
    value.len() == MAX_NUDGE_ID_BYTES
        && uuid::Uuid::parse_str(value).is_ok_and(|identifier| {
            !identifier.is_nil() && identifier.hyphenated().to_string() == value
        })
}

fn notification_user_info(nudge_id: &str) -> Option<Value> {
    valid_nudge_id(nudge_id).then(|| json!({"nudge_id": nudge_id}))
}

fn delivery_succeeded(in_app: bool, system: bool) -> bool {
    in_app || system
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(is_unsafe_scalar)
        && value.trim() == value
}

fn is_unsafe_scalar(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

fn valid_deep_link(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_DEEP_LINK_BYTES || value.chars().any(is_unsafe_scalar)
    {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    if url.scheme() != "woof"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return false;
    }

    let root_path = matches!(url.path(), "" | "/");
    match url.host_str() {
        Some("settings") => root_path && url.query().is_none(),
        Some("memory-hub") => {
            matches!(url.path(), "/followups" | "/workflows") && url.query().is_none()
        }
        Some("chat") if root_path => {
            let parameters = url.query_pairs().collect::<Vec<_>>();
            parameters.len() <= 1
                && parameters.iter().all(|(key, value)| {
                    key == "prompt"
                        && !value.is_empty()
                        && value.len() <= 1_000
                        && !value.chars().any(is_unsafe_scalar)
                })
        }
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeliveryError {
    Denied,
    Unavailable,
    TimedOut,
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        ffi::CStr,
        sync::{Arc, Mutex, OnceLock},
    };

    use block2::{DynBlock, RcBlock};
    use objc2::{
        msg_send,
        rc::Retained,
        runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, Sel},
        sel,
    };
    use objc2_foundation::NSString;
    use tokio::sync::oneshot;

    use super::{
        notification_user_info, valid_nudge_id, DeliveryError, SYSTEM_NOTIFICATION_BODY,
        SYSTEM_NOTIFICATION_TITLE,
    };

    const CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    const AUTHORIZATION_OPTIONS: usize = (1 << 1) | (1 << 2);
    const FOREGROUND_PRESENTATION_OPTIONS: usize = (1 << 1) | (1 << 4);
    const USER_INFO_NUDGE_ID: &str = "nudge_id";
    static NOTIFICATION_APP: OnceLock<tauri::AppHandle> = OnceLock::new();
    static DELEGATE_POINTER: OnceLock<usize> = OnceLock::new();

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum AuthorizationStatus {
        NotDetermined,
        Denied,
        Authorized,
    }

    #[link(name = "UserNotifications", kind = "framework")]
    unsafe extern "C" {}

    pub fn install_delegate(app: tauri::AppHandle) -> Result<(), DeliveryError> {
        let _ = NOTIFICATION_APP.set(app);
        if DELEGATE_POINTER.get().is_some() {
            return Ok(());
        }

        let superclass = AnyClass::get(c"NSObject").ok_or(DeliveryError::Unavailable)?;
        let delegate_class = match AnyClass::get(c"woofNotificationCenterDelegate") {
            Some(class) => class,
            None => {
                let mut builder = ClassBuilder::new(c"woofNotificationCenterDelegate", superclass)
                    .ok_or(DeliveryError::Unavailable)?;
                if let Some(protocol) = AnyProtocol::get(c"UNUserNotificationCenterDelegate") {
                    builder.add_protocol(protocol);
                }
                // SAFETY: Both functions use the exact Objective-C delegate
                // method ABIs declared by UNUserNotificationCenterDelegate.
                unsafe {
                    builder.add_method::<AnyObject, _>(
                        sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:),
                        did_receive_response
                            as unsafe extern "C-unwind" fn(
                                *mut AnyObject,
                                Sel,
                                *mut AnyObject,
                                *mut AnyObject,
                                *mut DynBlock<dyn Fn()>,
                            ),
                    );
                    builder.add_method::<AnyObject, _>(
                        sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
                        will_present_notification
                            as unsafe extern "C-unwind" fn(
                                *mut AnyObject,
                                Sel,
                                *mut AnyObject,
                                *mut AnyObject,
                                *mut DynBlock<dyn Fn(usize)>,
                            ),
                    );
                }
                builder.register()
            }
        };

        // SAFETY: The dynamic class inherits NSObject and implements the two
        // delegate callbacks above. The leaked retain is intentional: the
        // framework's delegate property is weak and must remain live for the
        // process lifetime.
        let delegate: Retained<AnyObject> = unsafe { msg_send![delegate_class, new] };
        let center = notification_center()? as *mut AnyObject;
        unsafe {
            let _: () = msg_send![center, setDelegate: &*delegate];
        }
        let pointer = Retained::into_raw(delegate) as usize;
        let _ = DELEGATE_POINTER.set(pointer);
        Ok(())
    }

    unsafe extern "C-unwind" fn will_present_notification(
        _this: *mut AnyObject,
        _cmd: Sel,
        _center: *mut AnyObject,
        _notification: *mut AnyObject,
        completion: *mut DynBlock<dyn Fn(usize)>,
    ) {
        if let Some(completion) = unsafe { completion.as_ref() } {
            completion.call((FOREGROUND_PRESENTATION_OPTIONS,));
        }
    }

    unsafe extern "C-unwind" fn did_receive_response(
        _this: *mut AnyObject,
        _cmd: Sel,
        _center: *mut AnyObject,
        response: *mut AnyObject,
        completion: *mut DynBlock<dyn Fn()>,
    ) {
        let nudge_id = unsafe { response_nudge_id(response) };
        if let (Some(app), Some(nudge_id)) = (NOTIFICATION_APP.get(), nudge_id) {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                for attempt in 0..8 {
                    if super::open_nudge(&app, &nudge_id).await.is_ok() {
                        break;
                    }
                    if attempt < 7 {
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    }
                }
            });
        }
        if let Some(completion) = unsafe { completion.as_ref() } {
            completion.call(());
        }
    }

    unsafe fn response_nudge_id(response: *mut AnyObject) -> Option<String> {
        if response.is_null() {
            return None;
        }
        let notification: *mut AnyObject = unsafe { msg_send![response, notification] };
        let request: *mut AnyObject = unsafe { msg_send![notification, request] };
        let content: *mut AnyObject = unsafe { msg_send![request, content] };
        let user_info: *mut AnyObject = unsafe { msg_send![content, userInfo] };
        if user_info.is_null() {
            return None;
        }
        let key = NSString::from_str(USER_INFO_NUDGE_ID);
        let identifier: *mut AnyObject = unsafe { msg_send![user_info, objectForKey: &*key] };
        if identifier.is_null() {
            return None;
        }
        let string_class = AnyClass::get(c"NSString")?;
        let is_string: Bool = unsafe { msg_send![identifier, isKindOfClass: string_class] };
        if !is_string.as_bool() {
            return None;
        }
        let utf8: *const std::ffi::c_char = unsafe { msg_send![identifier, UTF8String] };
        if utf8.is_null() {
            return None;
        }
        // SAFETY: NSString guarantees a NUL-terminated UTF-8 view for the
        // lifetime of the object. IDs are bounded immediately after copying.
        let value = unsafe { CStr::from_ptr(utf8) }.to_str().ok()?.to_owned();
        valid_nudge_id(&value).then_some(value)
    }

    pub async fn deliver(identifier: &str) -> Result<(), DeliveryError> {
        ensure_authorized().await?;
        add_request(identifier).await
    }

    async fn ensure_authorized() -> Result<(), DeliveryError> {
        match authorization_status().await? {
            AuthorizationStatus::Authorized => Ok(()),
            AuthorizationStatus::Denied => Err(DeliveryError::Denied),
            AuthorizationStatus::NotDetermined => {
                if request_authorization().await? {
                    match authorization_status().await? {
                        AuthorizationStatus::Authorized => Ok(()),
                        AuthorizationStatus::Denied | AuthorizationStatus::NotDetermined => {
                            Err(DeliveryError::Denied)
                        }
                    }
                } else {
                    Err(DeliveryError::Denied)
                }
            }
        }
    }

    async fn authorization_status() -> Result<AuthorizationStatus, DeliveryError> {
        let center = notification_center()?;
        let (sender, receiver) = oneshot::channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        {
            let completion_sender = Arc::clone(&sender);
            let completion: RcBlock<dyn Fn(*mut AnyObject)> =
                RcBlock::new(move |settings: *mut AnyObject| {
                    let status = if settings.is_null() {
                        None
                    } else {
                        // SAFETY: UserNotifications supplies a live
                        // UNNotificationSettings instance to this block.
                        Some(unsafe { msg_send![settings, authorizationStatus] })
                    };
                    if let Ok(mut sender) = completion_sender.lock() {
                        if let Some(sender) = sender.take() {
                            let _ = sender.send(status);
                        }
                    }
                });
            // SAFETY: `center` is the framework singleton, and the block has
            // the documented `void (^)(UNNotificationSettings *)` ABI.
            unsafe {
                let _: () = msg_send![
                    center as *mut AnyObject,
                    getNotificationSettingsWithCompletionHandler:
                        &*completion as &DynBlock<dyn Fn(*mut AnyObject)>
                ];
            }
        }

        let status = wait(receiver).await?.ok_or(DeliveryError::Unavailable)?;
        match status {
            0 => Ok(AuthorizationStatus::NotDetermined),
            1 => Ok(AuthorizationStatus::Denied),
            2..=4 => Ok(AuthorizationStatus::Authorized),
            _ => Err(DeliveryError::Unavailable),
        }
    }

    async fn request_authorization() -> Result<bool, DeliveryError> {
        let center = notification_center()?;
        let (sender, receiver) = oneshot::channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        {
            let completion_sender = Arc::clone(&sender);
            let completion: RcBlock<dyn Fn(Bool, *mut AnyObject)> =
                RcBlock::new(move |granted: Bool, error: *mut AnyObject| {
                    let result = error.is_null().then_some(granted.as_bool());
                    if let Ok(mut sender) = completion_sender.lock() {
                        if let Some(sender) = sender.take() {
                            let _ = sender.send(result);
                        }
                    }
                });
            // SAFETY: `center` is the framework singleton, and the block has
            // the documented `void (^)(BOOL, NSError *)` ABI.
            unsafe {
                let _: () = msg_send![
                    center as *mut AnyObject,
                    requestAuthorizationWithOptions: AUTHORIZATION_OPTIONS,
                    completionHandler:
                        &*completion as &DynBlock<dyn Fn(Bool, *mut AnyObject)>
                ];
            }
        }

        wait(receiver).await?.ok_or(DeliveryError::Unavailable)
    }

    async fn add_request(identifier: &str) -> Result<(), DeliveryError> {
        if notification_user_info(identifier).is_none() {
            return Err(DeliveryError::Unavailable);
        }
        let center = notification_center()?;
        let receiver = {
            let content_class =
                AnyClass::get(c"UNMutableNotificationContent").ok_or(DeliveryError::Unavailable)?;
            let request_class =
                AnyClass::get(c"UNNotificationRequest").ok_or(DeliveryError::Unavailable)?;
            let identifier = NSString::from_str(identifier);
            let title = NSString::from_str(SYSTEM_NOTIFICATION_TITLE);
            let body = NSString::from_str(SYSTEM_NOTIFICATION_BODY);
            let user_info_key = NSString::from_str(USER_INFO_NUDGE_ID);
            let dictionary_class =
                AnyClass::get(c"NSDictionary").ok_or(DeliveryError::Unavailable)?;
            let user_info: Retained<AnyObject> = unsafe {
                msg_send![
                    dictionary_class,
                    dictionaryWithObject: &*identifier,
                    forKey: &*user_info_key
                ]
            };

            // SAFETY: These are the documented UserNotifications constructors
            // and setters. `Retained` applies the correct ownership rules.
            let content: Retained<AnyObject> = unsafe { msg_send![content_class, new] };
            unsafe {
                let _: () = msg_send![&*content, setTitle: &*title];
                let _: () = msg_send![&*content, setBody: &*body];
                let _: () = msg_send![&*content, setUserInfo: &*user_info];
            }
            let request: Retained<AnyObject> = unsafe {
                msg_send![
                    request_class,
                    requestWithIdentifier: &*identifier,
                    content: &*content,
                    trigger: std::ptr::null::<AnyObject>()
                ]
            };

            let (sender, receiver) = oneshot::channel();
            let sender = Arc::new(Mutex::new(Some(sender)));
            let completion_sender = Arc::clone(&sender);
            let completion: RcBlock<dyn Fn(*mut AnyObject)> =
                RcBlock::new(move |error: *mut AnyObject| {
                    if let Ok(mut sender) = completion_sender.lock() {
                        if let Some(sender) = sender.take() {
                            let _ = sender.send(error.is_null());
                        }
                    }
                });
            // SAFETY: The notification center retains the request as needed,
            // and the block has the documented `void (^)(NSError *)` ABI.
            unsafe {
                let _: () = msg_send![
                    center as *mut AnyObject,
                    addNotificationRequest: &*request,
                    withCompletionHandler:
                        &*completion as &DynBlock<dyn Fn(*mut AnyObject)>
                ];
            }
            receiver
        };

        if wait(receiver).await? {
            Ok(())
        } else {
            Err(DeliveryError::Unavailable)
        }
    }

    fn notification_center() -> Result<usize, DeliveryError> {
        let center_class =
            AnyClass::get(c"UNUserNotificationCenter").ok_or(DeliveryError::Unavailable)?;
        // SAFETY: This is the documented singleton constructor.
        let center: *mut AnyObject = unsafe { msg_send![center_class, currentNotificationCenter] };
        if center.is_null() {
            Err(DeliveryError::Unavailable)
        } else {
            Ok(center as usize)
        }
    }

    async fn wait<T>(receiver: oneshot::Receiver<T>) -> Result<T, DeliveryError> {
        match tokio::time::timeout(CALLBACK_TIMEOUT, receiver).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(DeliveryError::Unavailable),
            Err(_) => Err(DeliveryError::TimedOut),
        }
    }
}

#[cfg(target_os = "macos")]
async fn deliver(identifier: &str) -> Result<(), DeliveryError> {
    platform::deliver(identifier).await
}

#[cfg(target_os = "macos")]
fn install_delegate(app: tauri::AppHandle) -> Result<(), DeliveryError> {
    platform::install_delegate(app)
}

#[cfg(not(target_os = "macos"))]
async fn deliver(_identifier: &str) -> Result<(), DeliveryError> {
    Err(DeliveryError::Unavailable)
}

#[cfg(not(target_os = "macos"))]
fn install_delegate(_app: tauri::AppHandle) -> Result<(), DeliveryError> {
    Err(DeliveryError::Unavailable)
}

fn emit_to_companion(app: &tauri::AppHandle, event: &str, payload: Value) -> bool {
    use tauri::Emitter;

    if let Some(companion) = app.get_webview_window(crate::companion_panel::WINDOW_LABEL) {
        companion.emit(event, payload).is_ok()
    } else {
        false
    }
}

fn emit_nudge_to_companion(app: &tauri::AppHandle, nudge: &ValidatedNudge) -> bool {
    emit_to_companion(
        app,
        "woof:nudge-ready",
        json!({
            "nudge_id": nudge.id,
            "title": nudge.title,
            "body": nudge.body,
            "deep_link": nudge.deep_link,
        }),
    )
}

async fn mark_delivery(nudge_id: &str) -> Result<(), String> {
    crate::commands::daemon_request(
        Method::POST,
        "/nudges/mark-delivered",
        Some(json!({"nudge_id": nudge_id})),
    )
    .await
    .map(|_| ())
}

pub(crate) async fn open_nudge(app: &tauri::AppHandle, nudge_id: &str) -> Result<(), String> {
    if !valid_nudge_id(nudge_id) {
        return Err("invalid nudge ID".into());
    }
    let response = crate::commands::daemon_request(
        Method::GET,
        &format!("/nudges/item?nudge_id={nudge_id}"),
        None,
    )
    .await?;
    let nudge = response
        .get("nudge")
        .and_then(ValidatedNudge::from_json)
        .ok_or_else(|| "woof’s local service returned an invalid nudge".to_string())?;

    let routed = nudge.deep_link.as_deref().is_some_and(|deep_link| {
        let Ok(url) = url::Url::parse(deep_link) else {
            return false;
        };
        if url.host_str() != Some("memory-hub")
            && crate::commands::open_companion_focused(app).is_err()
        {
            return false;
        }
        crate::commands::handle_woof_deep_link(app, &url)
    });

    if routed {
        crate::commands::daemon_request(
            Method::POST,
            "/nudges/mark-seen",
            Some(json!({"nudge_id": nudge.id})),
        )
        .await?;
        return Ok(());
    }

    crate::commands::open_companion_focused(app)?;
    if !emit_nudge_to_companion(app, &nudge) {
        return Err("could not restore the nudge in woof".into());
    }
    mark_delivery(&nudge.id).await
}

pub fn start(app: tauri::AppHandle) {
    if install_delegate(app.clone()).is_err() {
        emit_to_companion(
            &app,
            "woof:notification-status",
            json!({"status": "failed"}),
        );
    }

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        loop {
            let enabled =
                crate::commands::daemon_request(Method::GET, "/preferences/nudges-enabled", None)
                    .await
                    .ok()
                    .and_then(|value| value.get("enabled").and_then(Value::as_bool))
                    .unwrap_or(false);
            if enabled {
                if let Ok(response) = crate::commands::daemon_request(
                    Method::GET,
                    "/nudges/ready-unseen?limit=8",
                    None,
                )
                .await
                {
                    for value in response
                        .get("nudges")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let Some(nudge) = ValidatedNudge::from_json(value) else {
                            continue;
                        };
                        let in_app_delivered = emit_nudge_to_companion(&app, &nudge);
                        let system_delivered = if nudge.delivered_at.is_none() {
                            match deliver(&nudge.id).await {
                                Ok(()) => true,
                                Err(DeliveryError::Denied) => {
                                    emit_to_companion(
                                        &app,
                                        "woof:notification-status",
                                        json!({"status": "denied"}),
                                    );
                                    false
                                }
                                Err(DeliveryError::Unavailable | DeliveryError::TimedOut) => {
                                    emit_to_companion(
                                        &app,
                                        "woof:notification-status",
                                        json!({"status": "failed"}),
                                    );
                                    false
                                }
                            }
                        } else {
                            false
                        };
                        if nudge.delivered_at.is_none()
                            && delivery_succeeded(in_app_delivered, system_delivered)
                            && mark_delivery(&nudge.id).await.is_err()
                        {
                            emit_to_companion(
                                &app,
                                "woof:notification-status",
                                json!({"status": "failed"}),
                            );
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn validates_supported_deep_links_only() {
        for link in [
            "woof://settings",
            "woof://memory-hub/followups",
            "woof://memory-hub/workflows",
            "woof://chat?prompt=Review%20today",
        ] {
            assert!(valid_deep_link(link), "{link}");
        }
        for link in [
            "https://example.com",
            "woof://settings/other",
            "woof://chat?other=value",
            "woof://chat?prompt=one&prompt=two",
            "woof://memory-hub/followups?prompt=x",
            "woof://user@settings",
            "woof://chat?prompt=%E2%80%AEhidden",
        ] {
            assert!(!valid_deep_link(link), "{link}");
        }
    }

    #[test]
    fn rejects_control_and_bidi_content() {
        for unsafe_text in ["line\nbreak", "hidden\u{202e}text", "hidden\u{2066}text"] {
            assert!(!valid_text(unsafe_text, 256));
        }
        assert!(valid_text("A safe reminder", 256));
    }

    #[test]
    fn validates_complete_nudge_payload() {
        let nudge_id = "0194f3cb-16d8-7f10-a922-4379a7c54d31";
        let valid = json!({
            "nudge_id": nudge_id,
            "title": "Follow up",
            "body": "Review the draft",
            "deep_link": "woof://chat?prompt=Review%20the%20draft",
            "sent_at": null,
        });
        let nudge = ValidatedNudge::from_json(&valid).expect("valid nudge");
        assert_eq!(nudge.id, nudge_id);
        assert_eq!(nudge.delivered_at, None);

        let mut invalid = valid;
        invalid["deep_link"] = Value::String("file:///tmp/private".into());
        assert!(ValidatedNudge::from_json(&invalid).is_some_and(|nudge| nudge.deep_link.is_none()));
    }

    #[test]
    fn system_notification_payload_contains_only_an_opaque_nudge_id() {
        let nudge_id = "0194f3cb-16d8-7f10-a922-4379a7c54d31";
        let user_info = notification_user_info(nudge_id).expect("opaque notification payload");
        assert_eq!(user_info, json!({"nudge_id": nudge_id}));
        let serialized = user_info.to_string();
        for private_field in ["title", "body", "deep_link", "prompt", "captured"] {
            assert!(!serialized.contains(private_field));
        }
        assert!(notification_user_info("nudge-42").is_none());
    }

    #[test]
    fn delivery_failure_never_qualifies_a_nudge_for_acknowledgement() {
        assert!(!delivery_succeeded(false, false));
        assert!(delivery_succeeded(true, false));
        assert!(delivery_succeeded(false, true));
    }
}
