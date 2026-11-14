use super::*;

#[test]
fn blob_download_activation_retains_bytes_after_revocation_and_creator_retirement() {
    for tag in ["a", "area"] {
        for child in [false, true] {
            let mut vm = new_parsed_test_vm(
                "https://blob-download.test/",
                "<!doctype html><body></body>",
            );
            let url = vm.eval(&format!(r#"(() => {{
                const frame = {child} ? document.body.appendChild(document.createElement('iframe')) : null;
                const w = frame ? frame.contentWindow : window;
                const url = w.URL.createObjectURL(new w.Blob([new w.Uint8Array([0, 255, 128, 97, 13, 10])], {{type:'application/octet-stream'}}));
                const link = w.document.body.appendChild(w.document.createElement({tag:?}));
                link.href = url; link.download = 'bytes.bin'; link.click();
                w.URL.revokeObjectURL(url);
                if (frame) frame.remove();
                return url;
            }})()"#)).unwrap();
            let mut activations = vm.take_pending_download_activations();
            assert_eq!(activations.len(), 1, "{tag} child={child}");
            let activation = activations.pop().unwrap();
            assert_eq!(activation.url, url);
            assert_eq!(activation.suggested_filename.as_deref(), Some("bytes.bin"));
            assert!(crate::blob::object_url_bytes_and_type(&url).is_none());
            let response = activation
                .response
                .expect("accepted Blob download must own its response");
            assert_eq!(response.final_url, url);
            assert_eq!(response.status, 200);
            assert!(
                response
                    .headers
                    .iter()
                    .any(|(name, value)| name.eq_ignore_ascii_case("Content-Type")
                        && value == b"application/octet-stream")
            );
            assert_eq!(
                response.body,
                crate::runtime::RendererDownloadBody::Buffered(vec![0, 255, 128, 97, 13, 10])
            );
            assert!(vm.take_pending_location_navigation_with_seed().is_none());
        }
    }
}

#[test]
fn canceled_and_previously_revoked_blob_downloads_do_not_capture_a_response() {
    for cancel in [false, true] {
        let mut vm = new_parsed_test_vm(
            "https://blob-download-cancel.test/",
            "<!doctype html><body></body>",
        );
        vm.eval(&format!(
            r#"(() => {{
            const link = document.body.appendChild(document.createElement('a'));
            const url = URL.createObjectURL(new Blob(['not accepted']));
            link.href = url; link.download = 'bytes.bin';
            if ({cancel}) link.addEventListener('click', e => e.preventDefault());
            URL.revokeObjectURL(url); link.click();
        }})()"#
        ))
        .unwrap();
        let activations = vm.take_pending_download_activations();
        if cancel {
            assert!(activations.is_empty());
        } else {
            assert_eq!(activations.len(), 1);
            assert!(activations[0].response.is_none());
        }
    }
}
