//! A named-pipe listener for the protocol router: current user only, local
//! clients only, one first instance, and a 32-byte token before any byte
//! reaches Moli.

use std::{io, net::SocketAddr, time::Duration};

use tokio::{
    io::AsyncReadExt,
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::mpsc,
};

pub const TOKEN_LEN: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const PENDING_CONNECTIONS: usize = 16;

pub struct PipeListener {
    accepted: mpsc::Receiver<NamedPipeServer>,
    local_addr: SocketAddr,
}

impl PipeListener {
    /// Creates the first pipe instance now, so a squatter fails here and not
    /// silently later, then accepts in the background.
    pub fn create(name: &str, token: [u8; TOKEN_LEN], local_addr: SocketAddr) -> io::Result<Self> {
        let security = security::CurrentUserOnly::new()?;
        let first = create_instance(name, &security, true)?;
        let (sender, accepted) = mpsc::channel(PENDING_CONNECTIONS);
        let name = name.to_owned();
        tokio::spawn(async move {
            let mut server = first;
            loop {
                if server.connect().await.is_err() {
                    match create_instance(&name, &security, false) {
                        Ok(next) => server = next,
                        Err(_) => return,
                    }
                    continue;
                }
                let next = match create_instance(&name, &security, false) {
                    Ok(next) => next,
                    Err(_) => return,
                };
                let connected = std::mem::replace(&mut server, next);
                let sender = sender.clone();
                tokio::spawn(async move {
                    if let Some(stream) = handshake(connected, token).await {
                        let _ = sender.send(stream).await;
                    }
                });
            }
        });
        Ok(Self { accepted, local_addr })
    }
}

fn create_instance(
    name: &str,
    security: &security::CurrentUserOnly,
    first: bool,
) -> io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options.first_pipe_instance(first).reject_remote_clients(true);
    // SAFETY: the attributes point at a descriptor owned by `security`, which
    // outlives every call; the OS copies the descriptor when the pipe is made.
    unsafe { options.create_with_security_attributes_raw(name, security.attributes()) }
}

async fn handshake(mut stream: NamedPipeServer, token: [u8; TOKEN_LEN]) -> Option<NamedPipeServer> {
    let mut presented = [0u8; TOKEN_LEN];
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.read_exact(&mut presented)).await {
        Ok(Ok(_)) if constant_time_eq(&presented, &token) => Some(stream),
        _ => {
            let _ = stream.disconnect();
            None
        }
    }
}

pub fn constant_time_eq(a: &[u8; TOKEN_LEN], b: &[u8; TOKEN_LEN]) -> bool {
    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b) {
        difference |= x ^ y;
    }
    std::hint::black_box(difference) == 0
}

impl axum::serve::Listener for PipeListener {
    type Io = NamedPipeServer;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.accepted.recv().await {
            Some(stream) => (stream, self.local_addr),
            // The accept loop ended: nothing more will connect.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local_addr)
    }
}

mod security {
    use std::{ffi::c_void, io, ptr};

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
            TOKEN_USER, TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    /// A protected DACL with one ACE: full access for the current user's SID.
    pub struct CurrentUserOnly {
        descriptor: PSECURITY_DESCRIPTOR,
        attributes: Box<SECURITY_ATTRIBUTES>,
    }

    // The descriptor is immutable after construction and only read by the OS.
    unsafe impl Send for CurrentUserOnly {}
    unsafe impl Sync for CurrentUserOnly {}

    impl CurrentUserOnly {
        pub fn new() -> io::Result<Self> {
            let sid = current_user_sid_string()?;
            let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
            // SAFETY: valid NUL-terminated SDDL and out pointer.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let attributes = Box::new(SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            });
            Ok(Self { descriptor, attributes })
        }

        pub fn attributes(&self) -> *mut c_void {
            (&*self.attributes as *const SECURITY_ATTRIBUTES).cast_mut().cast()
        }
    }

    impl Drop for CurrentUserOnly {
        fn drop(&mut self) {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe { LocalFree(self.descriptor) };
        }
    }

    pub fn current_user_sid_string() -> io::Result<String> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: pseudo-handle of the current process and a valid out pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let mut needed = 0u32;
            // SAFETY: size query with a null buffer.
            unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed) };
            let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
            // SAFETY: the buffer holds `needed` bytes, 8-byte aligned.
            if unsafe {
                GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), needed, &mut needed)
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: GetTokenInformation(TokenUser) wrote a TOKEN_USER at the start.
            let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
            let mut text: *mut u16 = ptr::null_mut();
            // SAFETY: the SID lives in `buffer` for this call.
            if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: NUL-terminated wide string allocated by the OS.
            let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
            let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
            unsafe { LocalFree(text.cast()) };
            Ok(sid)
        })();
        // SAFETY: token opened above.
        unsafe { CloseHandle(token) };
        result
    }
}

pub use security::current_user_sid_string;
