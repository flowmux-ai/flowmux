// SPDX-License-Identifier: GPL-3.0-or-later
use std::path::{Path, PathBuf};

fn resource_path(root: &Path, url_path: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let path = root.join(url_path.strip_prefix('/')?).canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSData, NSError, NSObject, NSObjectProtocol, NSString, NSURLResponse};
use objc2_web_kit::{WKURLSchemeHandler, WKURLSchemeTask, WKWebView};
use std::cell::RefCell;

#[derive(Default)]
pub(super) struct DocumentResources {
    root: Option<PathBuf>,
    html: String,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "FlowmuxMarkdownResources"]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<DocumentResources>]
    pub(super) struct Resources;

    unsafe impl NSObjectProtocol for Resources {}
    unsafe impl WKURLSchemeHandler for Resources {
        #[unsafe(method(webView:startURLSchemeTask:))]
        unsafe fn start(&self, _web: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            let url = task.request().URL();
            let resource = url.as_ref().and_then(|url| url.path()).and_then(|path| {
                let document = self.ivars().borrow();
                if path.to_string() == "/" {
                    return Some((document.html.as_bytes().to_vec(), "text/html".to_string()));
                }
                let path = resource_path(document.root.as_ref()?, &path.to_string())?;
                let data = std::fs::read(&path).ok()?;
                let (kind, _) = gtk::gio::content_type_guess(Some(&path), &data);
                let mime = gtk::gio::content_type_get_mime_type(&kind).unwrap_or_else(|| "application/octet-stream".into());
                Some((data, mime.to_string()))
            });
            if let (Some(url), Some((data, mime))) = (url, resource) {
                let response = NSURLResponse::initWithURL_MIMEType_expectedContentLength_textEncodingName(
                    NSURLResponse::alloc(), &url, Some(&NSString::from_str(&mime)), data.len() as isize, None);
                task.didReceiveResponse(&response);
                task.didReceiveData(&NSData::with_bytes(&data));
                task.didFinish();
            } else {
                task.didFailWithError(&NSError::errorWithDomain_code_userInfo(&NSString::from_str("NSURLErrorDomain"), -1100, None));
            }
        }
        #[unsafe(method(webView:stopURLSchemeTask:))]
        unsafe fn stop(&self, _web: &WKWebView, _task: &ProtocolObject<dyn WKURLSchemeTask>) {
            // Reads finish synchronously; no callbacks remain after cancellation.
        }
    }
);

impl Resources {
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe {
            msg_send![
                super(Self::alloc(mtm).set_ivars(RefCell::new(DocumentResources::default()))),
                init
            ]
        }
    }

    pub(super) fn set_document(&self, html: String, root: Option<PathBuf>) {
        *self.ivars().borrow_mut() = DocumentResources { root, html };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resources_stay_in_the_document_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("document");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("한글 image.svg"), "<svg/>").unwrap();
        std::fs::write(dir.path().join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret"), root.join("escape")).unwrap();
        assert_eq!(
            resource_path(&root, "/한글 image.svg"),
            Some(root.join("한글 image.svg").canonicalize().unwrap())
        );
        for path in [
            "/../secret",
            "/escape",
            "/",
            "/missing",
            "//etc/passwd",
            "/nul\0.png",
        ] {
            assert!(resource_path(&root, path).is_none(), "{path}");
        }
    }
}
