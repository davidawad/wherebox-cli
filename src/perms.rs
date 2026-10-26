//! Owner-only permissions for the database, its sidecars and its directory.
//!
//! * Unix: files are created 0600 and new directories are chmod 0700.
//! * Windows: new directories get a protected (non-inherited) DACL that grants
//!   full control to the current user and `SYSTEM` only, inherited by
//!   everything created inside; files we create get the same DACL, so a
//!   database in a shared directory is private too. `SYSTEM` stays because
//!   Windows services (backup, search indexer) run as it, as root does on Unix.
//!
//! [`inspect`] is the platform-aware check behind `doctor`'s permission rows.

use std::io;
use std::path::Path;

/// Result of inspecting a path's permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// Only the owner (plus root / SYSTEM / Administrators) can access it.
    Private(String),
    /// Other users can access it; the string says how.
    Shared(String),
}

/// Restrict a directory we just created to the current user.
pub fn restrict_dir(path: &Path) -> io::Result<()> {
    imp::restrict(path, true)
}

/// Restrict a file we just created to the current user. On Unix the mode is
/// set at creation (see [`crate::crypto::create_private`]), so this is a no-op.
pub fn restrict_file(path: &Path) -> io::Result<()> {
    imp::restrict(path, false)
}

/// Who can access `path`, or `None` if it cannot be inspected.
pub fn inspect(path: &Path) -> Option<Access> {
    imp::inspect(path)
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::Access;

    pub fn restrict(path: &Path, dir: bool) -> io::Result<()> {
        if dir {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    pub fn inspect(path: &Path) -> Option<Access> {
        let meta = std::fs::metadata(path).ok()?;
        let m = meta.permissions().mode() & 0o777;
        let expected = if meta.is_dir() { "700" } else { "600" };
        Some(if m & 0o077 != 0 {
            Access::Shared(format!("mode {m:o}; expected {expected}"))
        } else {
            Access::Private(format!("mode {m:o}"))
        })
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null_mut;

    use windows_sys::core::PWSTR;
    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_SUCCESS, HANDLE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::Access;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    /// Take ownership of a `LocalAlloc`ed UTF-16 string.
    unsafe fn take_wstr(p: PWSTR) -> String {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
        LocalFree(p.cast());
        s
    }

    /// The current user's SID as a string (`S-1-5-21-…`).
    fn user_sid() -> io::Result<String> {
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
            // u64 storage keeps TOKEN_USER suitably aligned.
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
            let err = io::Error::last_os_error();
            CloseHandle(token);
            if ok == 0 {
                return Err(err);
            }
            let user = &*(buf.as_ptr().cast::<TOKEN_USER>());
            let mut s: PWSTR = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(take_wstr(s))
        }
    }

    pub fn restrict(path: &Path, dir: bool) -> io::Result<()> {
        let sid = user_sid()?;
        // Protected DACL: full access for the user and SYSTEM, nothing inherited
        // from the parent. Directory ACEs are inherited by new children.
        let inherit = if dir { "OICI" } else { "" };
        let sddl = wide(format!("D:P(A;{inherit};FA;;;{sid})(A;{inherit};FA;;;SY)").as_ref());
        unsafe {
            let mut sd: PSECURITY_DESCRIPTOR = null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut())
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            let (mut present, mut defaulted, mut dacl) = (0, 0, null_mut::<ACL>());
            let res = if GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) == 0 {
                Err(io::Error::last_os_error())
            } else {
                let name = wide(path.as_os_str());
                match SetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    dacl,
                    null_mut(),
                ) {
                    // FAT/exFAT volumes have no ACLs; doctor reports such files as shared.
                    ERROR_SUCCESS | ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION => Ok(()),
                    e => Err(io::Error::from_raw_os_error(e as i32)),
                }
            };
            LocalFree(sd);
            res
        }
    }

    /// The DACL of `path` in SDDL form, e.g. `D:P(A;OICI;FA;;;S-1-5-21-…)`.
    fn dacl_sddl(path: &Path) -> io::Result<String> {
        let name = wide(path.as_os_str());
        unsafe {
            let mut sd: PSECURITY_DESCRIPTOR = null_mut();
            let e = GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut sd,
            );
            if e != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(e as i32));
            }
            let mut s: PWSTR = null_mut();
            let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut s,
                null_mut(),
            );
            let err = io::Error::last_os_error();
            LocalFree(sd);
            if ok == 0 {
                return Err(err);
            }
            Ok(take_wstr(s))
        }
    }

    pub fn inspect(path: &Path) -> Option<Access> {
        let sddl = dacl_sddl(path).ok()?;
        let user = user_sid().ok()?;
        Some(super::classify_sddl(&sddl, &user))
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::io;
    use std::path::Path;

    use super::Access;

    pub fn restrict(_: &Path, _: bool) -> io::Result<()> {
        Ok(())
    }
    pub fn inspect(_: &Path) -> Option<Access> {
        None
    }
}

/// Whether SDDL trustee `sid` is the current `user`. SDDL writes the local
/// built-in Administrator (RID 500) as the alias `LA` rather than its SID,
/// which is who CI runners and some single-user machines run as.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_user(sid: &str, user: &str) -> bool {
    sid == user || (sid == "LA" && user.starts_with("S-1-5-21-") && user.ends_with("-500"))
}

/// Classify a DACL in SDDL form: private when every allow ACE is for `user`
/// or a trustee that is all-powerful anyway (SYSTEM, Administrators, the
/// owner / creator-owner placeholders).
#[cfg_attr(not(windows), allow(dead_code))]
fn classify_sddl(sddl: &str, user: &str) -> Access {
    const TRUSTED: [&str; 4] = ["SY", "BA", "OW", "CO"];
    let others: Vec<&str> = sddl
        .split('(')
        .skip(1)
        .filter_map(|ace| {
            let f: Vec<&str> = ace.trim_end_matches(')').split(';').collect();
            let (kind, sid) = (*f.first()?, *f.get(5)?);
            (kind == "A" && !is_user(sid, user) && !TRUSTED.contains(&sid)).then_some(sid)
        })
        .collect();
    if sddl.contains("NO_ACCESS_CONTROL") {
        return Access::Shared("no ACL (everyone has access)".into());
    }
    if others.is_empty() {
        Access::Private("ACL grants only the current user, SYSTEM and Administrators".into())
    } else {
        Access::Shared(format!("ACL also grants access to {}", others.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "S-1-5-21-1-2-3-1001";

    #[test]
    fn sddl_classification() {
        let private = format!("D:P(A;OICI;FA;;;{ME})(A;OICI;FA;;;SY)");
        assert!(matches!(classify_sddl(&private, ME), Access::Private(_)));
        let profile = format!("D:(A;OICIID;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICIID;FA;;;{ME})");
        assert!(matches!(classify_sddl(&profile, ME), Access::Private(_)));
        let shared = format!("D:(A;OICIID;FA;;;{ME})(A;OICIID;0x1200a9;;;BU)(D;;FA;;;WD)");
        assert_eq!(classify_sddl(&shared, ME), Access::Shared("ACL also grants access to BU".into()));
        assert!(matches!(classify_sddl("D:NO_ACCESS_CONTROL", ME), Access::Shared(_)));
        let admin = "S-1-5-21-1-2-3-500";
        assert!(matches!(classify_sddl("D:P(A;OICI;FA;;;LA)(A;OICI;FA;;;SY)", admin), Access::Private(_)));
        assert!(matches!(classify_sddl("D:P(A;OICI;FA;;;LA)(A;OICI;FA;;;SY)", ME), Access::Shared(_)));
    }

    #[test]
    fn restricted_dir_is_private() {
        let t = tempfile::TempDir::new().unwrap();
        let d = t.path().join("private");
        std::fs::create_dir(&d).unwrap();
        restrict_dir(&d).unwrap();
        let f = d.join("file");
        crate::crypto::create_private(&f).unwrap();
        for p in [&d, &f] {
            if let Some(a) = inspect(p) {
                assert!(matches!(a, Access::Private(_)), "{}: {a:?}", p.display());
            }
        }
    }
}
