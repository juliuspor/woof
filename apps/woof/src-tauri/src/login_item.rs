#[cfg(target_os = "macos")]
mod platform {
    use std::ptr;

    use objc2::{
        msg_send,
        runtime::{AnyClass, AnyObject, Bool},
    };

    const STATUS_NOT_REGISTERED: isize = 0;
    const STATUS_ENABLED: isize = 1;
    const STATUS_REQUIRES_APPROVAL: isize = 2;
    const STATUS_NOT_FOUND: isize = 3;

    #[link(name = "ServiceManagement", kind = "framework")]
    unsafe extern "C" {}

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Status {
        NotRegistered,
        Enabled,
        RequiresApproval,
        NotFound,
    }

    impl Status {
        fn decode(value: isize) -> Result<Self, String> {
            match value {
                STATUS_NOT_REGISTERED => Ok(Self::NotRegistered),
                STATUS_ENABLED => Ok(Self::Enabled),
                STATUS_REQUIRES_APPROVAL => Ok(Self::RequiresApproval),
                STATUS_NOT_FOUND => Ok(Self::NotFound),
                _ => Err("macOS returned an unknown login item state".into()),
            }
        }
    }

    fn main_app_service() -> Result<&'static AnyObject, String> {
        let class = AnyClass::get(c"SMAppService")
            .ok_or_else(|| "macOS Service Management is unavailable".to_string())?;
        // SAFETY: `mainAppService` is a macOS 13+ SMAppService class property.
        // The application requires macOS 14, and the returned singleton remains
        // valid for the duration of the process.
        let service: *mut AnyObject = unsafe { msg_send![class, mainAppService] };
        // SAFETY: Objective-C returned either a valid object or nil.
        unsafe { service.as_ref() }
            .ok_or_else(|| "macOS could not create the main login item service".to_string())
    }

    fn status(service: &AnyObject) -> Result<Status, String> {
        // SAFETY: `service` is an SMAppService and `status` returns NSInteger.
        let value: isize = unsafe { msg_send![service, status] };
        Status::decode(value)
    }

    fn register(service: &AnyObject) -> Result<(), String> {
        let mut error: *mut AnyObject = ptr::null_mut();
        // SAFETY: the selector and out-parameter match
        // `-[SMAppService registerAndReturnError:]`.
        let succeeded: Bool = unsafe { msg_send![service, registerAndReturnError: &mut error] };
        if succeeded.as_bool() {
            Ok(())
        } else {
            Err("macOS could not register woof as a login item".into())
        }
    }

    fn unregister(service: &AnyObject) -> Result<(), String> {
        let mut error: *mut AnyObject = ptr::null_mut();
        // SAFETY: the selector and out-parameter match
        // `-[SMAppService unregisterAndReturnError:]`.
        let succeeded: Bool = unsafe { msg_send![service, unregisterAndReturnError: &mut error] };
        if succeeded.as_bool() {
            Ok(())
        } else if status(service)? == Status::NotRegistered {
            // Another process may have removed the item between the status
            // query and this call. The requested state has still been reached.
            Ok(())
        } else {
            Err("macOS could not unregister the woof login item".into())
        }
    }

    pub fn is_enabled() -> Result<bool, String> {
        Ok(status(main_app_service()?)? == Status::Enabled)
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        let service = main_app_service()?;
        let before = status(service)?;
        if enabled {
            match before {
                Status::Enabled => return Ok(()),
                Status::NotRegistered => register(service)?,
                Status::RequiresApproval => {
                    return Err(
                        "allow woof under System Settings > General > Login Items, then try again"
                            .into(),
                    )
                }
                Status::NotFound => {
                    return Err("macOS could not find the woof application login item".into())
                }
            }
            match status(service)? {
                Status::Enabled => Ok(()),
                Status::RequiresApproval => Err(
                    "allow woof under System Settings > General > Login Items, then try again"
                        .into(),
                ),
                _ => Err("macOS did not enable the woof login item".into()),
            }
        } else {
            match before {
                Status::NotRegistered => Ok(()),
                Status::Enabled | Status::RequiresApproval => unregister(service),
                Status::NotFound => {
                    Err("macOS could not find the woof application login item".into())
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn decodes_every_documented_service_management_status() {
            assert_eq!(
                Status::decode(STATUS_NOT_REGISTERED).unwrap(),
                Status::NotRegistered
            );
            assert_eq!(Status::decode(STATUS_ENABLED).unwrap(), Status::Enabled);
            assert_eq!(
                Status::decode(STATUS_REQUIRES_APPROVAL).unwrap(),
                Status::RequiresApproval
            );
            assert_eq!(Status::decode(STATUS_NOT_FOUND).unwrap(), Status::NotFound);
            assert!(Status::decode(99).is_err());
        }
    }
}

#[cfg(target_os = "macos")]
pub use platform::{is_enabled, set_enabled};

#[cfg(not(target_os = "macos"))]
pub fn is_enabled() -> Result<bool, String> {
    Err("login items are supported only on macOS".into())
}

#[cfg(not(target_os = "macos"))]
pub fn set_enabled(_enabled: bool) -> Result<(), String> {
    Err("login items are supported only on macOS".into())
}
