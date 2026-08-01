#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicrophoneAuthorization {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use block2::{DynBlock, RcBlock};
    use objc2::{
        msg_send,
        runtime::{AnyClass, AnyObject, Bool},
    };
    use tokio::sync::oneshot;

    use super::MicrophoneAuthorization;
    use crate::{AudioError, CancellationToken};

    const PERMISSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {
        static AVMediaTypeAudio: *const AnyObject;
    }

    pub fn query() -> Result<MicrophoneAuthorization, AudioError> {
        let capture_device =
            AnyClass::get(c"AVCaptureDevice").ok_or(AudioError::PermissionQuery)?;
        // SAFETY: AVMediaTypeAudio is supplied by AVFoundation and the class
        // selector returns an AVAuthorizationStatus integer.
        let status: isize = unsafe {
            msg_send![
                capture_device,
                authorizationStatusForMediaType: AVMediaTypeAudio
            ]
        };
        match status {
            0 => Ok(MicrophoneAuthorization::NotDetermined),
            1 => Ok(MicrophoneAuthorization::Restricted),
            2 => Ok(MicrophoneAuthorization::Denied),
            3 => Ok(MicrophoneAuthorization::Authorized),
            _ => Err(AudioError::PermissionQuery),
        }
    }

    pub async fn request(
        cancellation: &CancellationToken,
    ) -> Result<MicrophoneAuthorization, AudioError> {
        if cancellation.is_cancelled() {
            return Err(AudioError::Cancelled);
        }
        match query()? {
            MicrophoneAuthorization::NotDetermined => {}
            status => return Ok(status),
        }

        let capture_device =
            AnyClass::get(c"AVCaptureDevice").ok_or(AudioError::PermissionRequest)?;
        let (sender, receiver) = oneshot::channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        {
            let completion_sender = Arc::clone(&sender);
            let completion: RcBlock<dyn Fn(Bool)> = RcBlock::new(move |granted: Bool| {
                if let Ok(mut sender) = completion_sender.lock() {
                    if let Some(sender) = sender.take() {
                        let _ = sender.send(granted.as_bool());
                    }
                }
            });

            // SAFETY: the block has the exact AVFoundation completion signature
            // `void (^)(BOOL)`. RcBlock is heap-backed and AVFoundation retains it
            // for the asynchronous authorization request.
            unsafe {
                let _: () = msg_send![
                    capture_device,
                    requestAccessForMediaType: AVMediaTypeAudio,
                    completionHandler: &*completion as &DynBlock<dyn Fn(Bool)>
                ];
            }
        }

        let granted =
            wait_for_authorization(receiver, cancellation, PERMISSION_REQUEST_TIMEOUT).await?;
        if granted {
            Ok(MicrophoneAuthorization::Authorized)
        } else {
            Ok(MicrophoneAuthorization::Denied)
        }
    }

    async fn wait_for_authorization(
        receiver: oneshot::Receiver<bool>,
        cancellation: &CancellationToken,
        timeout: Duration,
    ) -> Result<bool, AudioError> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(AudioError::Cancelled),
            result = tokio::time::timeout(timeout, receiver) => match result {
                Ok(Ok(granted)) => Ok(granted),
                Ok(Err(_)) => Err(AudioError::PermissionRequest),
                Err(_) => Err(AudioError::PermissionRequestTimeout),
            },
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn permission_wait_is_cancellation_ready() {
            let (_sender, receiver) = oneshot::channel();
            let cancellation = CancellationToken::new();
            cancellation.cancel();

            let result =
                wait_for_authorization(receiver, &cancellation, Duration::from_secs(1)).await;

            assert!(matches!(result, Err(AudioError::Cancelled)));
        }

        #[tokio::test]
        async fn permission_wait_is_bounded() {
            let (_sender, receiver) = oneshot::channel();
            let cancellation = CancellationToken::new();

            let result =
                wait_for_authorization(receiver, &cancellation, Duration::from_millis(1)).await;

            assert!(matches!(result, Err(AudioError::PermissionRequestTimeout)));
        }
    }
}

pub fn microphone_authorization() -> Result<MicrophoneAuthorization, crate::AudioError> {
    #[cfg(target_os = "macos")]
    {
        platform::query()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(crate::AudioError::UnsupportedPlatform)
    }
}

pub async fn request_microphone_authorization() -> Result<MicrophoneAuthorization, crate::AudioError>
{
    request_microphone_authorization_with_cancellation(&crate::CancellationToken::new()).await
}

/// Requests microphone authorization with a bounded wait and caller cancellation.
pub async fn request_microphone_authorization_with_cancellation(
    cancellation: &crate::CancellationToken,
) -> Result<MicrophoneAuthorization, crate::AudioError> {
    if cancellation.is_cancelled() {
        return Err(crate::AudioError::Cancelled);
    }
    #[cfg(target_os = "macos")]
    {
        platform::request(cancellation).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(crate::AudioError::UnsupportedPlatform)
    }
}
