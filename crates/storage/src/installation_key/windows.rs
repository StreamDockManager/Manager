//! Inspect the DACL and link count on the already-open key file.
use std::{
    fs::File,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
        *,
    },
    Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
    },
    System::{
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};
pub(super) fn private(file: &File) -> bool {
    // All SID/ACL pointers below borrow the OS-allocated descriptor until LocalFree.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0
            || info.nNumberOfLinks != 1
            || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return false;
        }
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let token = OwnedHandle::from_raw_handle(token);
        let mut needed = 0;
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut needed,
        );
        if needed == 0 || needed > 65536 {
            return false;
        }
        let mut user = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            user.as_mut_ptr().cast(),
            needed,
            &mut needed,
        ) == 0
        {
            return false;
        }
        let user = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        let mut acl = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        let mut owner = ptr::null_mut();
        if GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut descriptor,
        ) != 0
        {
            return false;
        }
        let valid = !acl.is_null()
            && !owner.is_null()
            && EqualSid(owner, user) != 0
            && (0..(*acl).AceCount).all(|index| {
                let mut ace = ptr::null_mut();
                if GetAce(acl, index.into(), &mut ace) == 0 || ace.is_null() {
                    return false;
                }
                match u32::from((*ace.cast::<ACE_HEADER>()).AceType) {
                    ACCESS_DENIED_ACE_TYPE => true,
                    ACCESS_ALLOWED_ACE_TYPE => {
                        let sid =
                            ptr::addr_of_mut!((*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast();
                        EqualSid(sid, user) != 0
                            || IsWellKnownSid(sid, WinLocalSystemSid) != 0
                            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
                    }
                    _ => false,
                }
            });
        LocalFree(descriptor);
        valid
    }
}
