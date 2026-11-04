#[doc(hidden)]
#[allow(non_snake_case, unused_unsafe)]
pub unsafe fn _export_on_layout_change_cabi<T_: Guest>() {
    unsafe {
        #[cfg(target_arch = "wasm32")] _rt::run_ctors_once();
        { T_::on_layout_change() };
    }
}
pub trait Guest {
    /// Guest entry point, called by the host after every layout mutation batch.
    /// Guests may call `host` getters/setters from inside this function.
    #[allow(async_fn_in_trait)]
    fn on_layout_change() -> ();
}
#[doc(hidden)]
macro_rules! __export_world_plugin_cabi {
    ($ty:ident with_types_in $($path_to_types:tt)*) => {
        const _ : () = { #[unsafe (export_name = "on-layout-change")] unsafe extern "C"
        fn export_on_layout_change() { unsafe { $($path_to_types)*::
        _export_on_layout_change_cabi::<$ty > () } } };
    };
}
#[doc(hidden)]
pub(crate) use __export_world_plugin_cabi;
#[allow(dead_code, clippy::all)]
pub mod berth {
    pub mod plugin {
        /// Everything a plugin can read or do on the layout (tier 1: layout commands).
        /// Host state is a read-only snapshot during `on-layout-change`; writes go
        /// through explicit setters which the app applies after the callback returns.
        #[allow(dead_code, async_fn_in_trait, unused_imports, clippy::all)]
        pub mod host {
            #[used]
            #[doc(hidden)]
            static __FORCE_SECTION_REF: fn() = super::super::super::__link_custom_section_describing_imports;
            use super::super::super::_rt;
            #[derive(Clone)]
            pub struct WorkspaceInfo {
                pub id: u64,
                pub label: _rt::String,
                pub active: bool,
            }
            impl ::core::fmt::Debug for WorkspaceInfo {
                fn fmt(
                    &self,
                    f: &mut ::core::fmt::Formatter<'_>,
                ) -> ::core::fmt::Result {
                    f.debug_struct("WorkspaceInfo")
                        .field("id", &self.id)
                        .field("label", &self.label)
                        .field("active", &self.active)
                        .finish()
                }
            }
            #[derive(Clone)]
            pub struct TabInfo {
                pub id: u64,
                pub workspace_id: u64,
                pub label: _rt::String,
                pub active: bool,
            }
            impl ::core::fmt::Debug for TabInfo {
                fn fmt(
                    &self,
                    f: &mut ::core::fmt::Formatter<'_>,
                ) -> ::core::fmt::Result {
                    f.debug_struct("TabInfo")
                        .field("id", &self.id)
                        .field("workspace-id", &self.workspace_id)
                        .field("label", &self.label)
                        .field("active", &self.active)
                        .finish()
                }
            }
            #[allow(unused_unsafe, clippy::all)]
            #[allow(async_fn_in_trait)]
            pub fn get_workspaces() -> _rt::Vec<WorkspaceInfo> {
                unsafe {
                    #[cfg_attr(target_pointer_width = "64", repr(align(8)))]
                    #[cfg_attr(target_pointer_width = "32", repr(align(4)))]
                    struct RetArea(
                        [::core::mem::MaybeUninit<
                            u8,
                        >; 2 * ::core::mem::size_of::<*const u8>()],
                    );
                    let mut ret_area = RetArea(
                        [::core::mem::MaybeUninit::uninit(); 2
                            * ::core::mem::size_of::<*const u8>()],
                    );
                    let ptr0 = ret_area.0.as_mut_ptr().cast::<u8>();
                    #[cfg(target_arch = "wasm32")]
                    #[link(wasm_import_module = "berth:plugin/host@0.1.0")]
                    unsafe extern "C" {
                        #[link_name = "get-workspaces"]
                        fn wit_import1(_: *mut u8);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    unsafe extern "C" fn wit_import1(_: *mut u8) {
                        unreachable!()
                    }
                    wit_import1(ptr0);
                    let l2 = *ptr0.add(0).cast::<*mut u8>();
                    let l3 = *ptr0
                        .add(::core::mem::size_of::<*const u8>())
                        .cast::<usize>();
                    let base9 = l2;
                    let len9 = l3;
                    let mut result9 = _rt::Vec::with_capacity(len9);
                    for i in 0..len9 {
                        let base = base9
                            .add(i * (16 + 2 * ::core::mem::size_of::<*const u8>()));
                        let e9 = {
                            let l4 = *base.add(0).cast::<i64>();
                            let l5 = *base.add(8).cast::<*mut u8>();
                            let l6 = *base
                                .add(8 + 1 * ::core::mem::size_of::<*const u8>())
                                .cast::<usize>();
                            let len7 = l6;
                            let bytes7 = _rt::Vec::from_raw_parts(l5.cast(), len7, len7);
                            let l8 = i32::from(
                                *base
                                    .add(8 + 2 * ::core::mem::size_of::<*const u8>())
                                    .cast::<u8>(),
                            );
                            WorkspaceInfo {
                                id: l4 as u64,
                                label: _rt::string_lift(bytes7),
                                active: _rt::bool_lift(l8 as u8),
                            }
                        };
                        result9.push(e9);
                    }
                    _rt::cabi_dealloc(
                        base9,
                        len9 * (16 + 2 * ::core::mem::size_of::<*const u8>()),
                        8,
                    );
                    let result10 = result9;
                    result10
                }
            }
            #[allow(unused_unsafe, clippy::all)]
            #[allow(async_fn_in_trait)]
            pub fn get_tabs() -> _rt::Vec<TabInfo> {
                unsafe {
                    #[cfg_attr(target_pointer_width = "64", repr(align(8)))]
                    #[cfg_attr(target_pointer_width = "32", repr(align(4)))]
                    struct RetArea(
                        [::core::mem::MaybeUninit<
                            u8,
                        >; 2 * ::core::mem::size_of::<*const u8>()],
                    );
                    let mut ret_area = RetArea(
                        [::core::mem::MaybeUninit::uninit(); 2
                            * ::core::mem::size_of::<*const u8>()],
                    );
                    let ptr0 = ret_area.0.as_mut_ptr().cast::<u8>();
                    #[cfg(target_arch = "wasm32")]
                    #[link(wasm_import_module = "berth:plugin/host@0.1.0")]
                    unsafe extern "C" {
                        #[link_name = "get-tabs"]
                        fn wit_import1(_: *mut u8);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    unsafe extern "C" fn wit_import1(_: *mut u8) {
                        unreachable!()
                    }
                    wit_import1(ptr0);
                    let l2 = *ptr0.add(0).cast::<*mut u8>();
                    let l3 = *ptr0
                        .add(::core::mem::size_of::<*const u8>())
                        .cast::<usize>();
                    let base10 = l2;
                    let len10 = l3;
                    let mut result10 = _rt::Vec::with_capacity(len10);
                    for i in 0..len10 {
                        let base = base10
                            .add(i * (24 + 2 * ::core::mem::size_of::<*const u8>()));
                        let e10 = {
                            let l4 = *base.add(0).cast::<i64>();
                            let l5 = *base.add(8).cast::<i64>();
                            let l6 = *base.add(16).cast::<*mut u8>();
                            let l7 = *base
                                .add(16 + 1 * ::core::mem::size_of::<*const u8>())
                                .cast::<usize>();
                            let len8 = l7;
                            let bytes8 = _rt::Vec::from_raw_parts(l6.cast(), len8, len8);
                            let l9 = i32::from(
                                *base
                                    .add(16 + 2 * ::core::mem::size_of::<*const u8>())
                                    .cast::<u8>(),
                            );
                            TabInfo {
                                id: l4 as u64,
                                workspace_id: l5 as u64,
                                label: _rt::string_lift(bytes8),
                                active: _rt::bool_lift(l9 as u8),
                            }
                        };
                        result10.push(e10);
                    }
                    _rt::cabi_dealloc(
                        base10,
                        len10 * (24 + 2 * ::core::mem::size_of::<*const u8>()),
                        8,
                    );
                    let result11 = result10;
                    result11
                }
            }
            #[allow(unused_unsafe, clippy::all)]
            /// Queue a workspace rename; applied by the host after the callback returns
            /// (this way plugins cannot observe torn state mid-guest-call).
            #[allow(async_fn_in_trait)]
            pub fn set_workspace_label(id: u64, label: &str) -> () {
                unsafe {
                    let vec0 = label;
                    let ptr0 = vec0.as_ptr().cast::<u8>();
                    let len0 = vec0.len();
                    #[cfg(target_arch = "wasm32")]
                    #[link(wasm_import_module = "berth:plugin/host@0.1.0")]
                    unsafe extern "C" {
                        #[link_name = "set-workspace-label"]
                        fn wit_import1(_: i64, _: *mut u8, _: usize);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    unsafe extern "C" fn wit_import1(_: i64, _: *mut u8, _: usize) {
                        unreachable!()
                    }
                    wit_import1(_rt::as_i64(&id), ptr0.cast_mut(), len0);
                }
            }
        }
    }
}
mod _rt {
    #![allow(dead_code, unused_imports, clippy::all)]
    pub use alloc_crate::string::String;
    pub use alloc_crate::vec::Vec;
    pub unsafe fn string_lift(bytes: Vec<u8>) -> String {
        if cfg!(debug_assertions) {
            String::from_utf8(bytes).unwrap()
        } else {
            unsafe { String::from_utf8_unchecked(bytes) }
        }
    }
    pub unsafe fn bool_lift(val: u8) -> bool {
        if cfg!(debug_assertions) {
            match val {
                0 => false,
                1 => true,
                _ => panic!("invalid bool discriminant"),
            }
        } else {
            val != 0
        }
    }
    pub unsafe fn cabi_dealloc(ptr: *mut u8, size: usize, align: usize) {
        if size == 0 {
            return;
        }
        unsafe {
            let layout = alloc::Layout::from_size_align_unchecked(size, align);
            alloc::dealloc(ptr, layout);
        }
    }
    pub fn as_i64<T: AsI64>(t: T) -> i64 {
        t.as_i64()
    }
    pub trait AsI64 {
        fn as_i64(self) -> i64;
    }
    impl<'a, T: Copy + AsI64> AsI64 for &'a T {
        fn as_i64(self) -> i64 {
            (*self).as_i64()
        }
    }
    impl AsI64 for i64 {
        #[inline]
        fn as_i64(self) -> i64 {
            self as i64
        }
    }
    impl AsI64 for u64 {
        #[inline]
        fn as_i64(self) -> i64 {
            self as i64
        }
    }
    #[cfg(target_arch = "wasm32")]
    pub fn run_ctors_once() {
        wit_bindgen::rt::run_ctors_once();
    }
    extern crate alloc as alloc_crate;
    pub use alloc_crate::alloc;
}
/// Generates `#[unsafe(no_mangle)]` functions to export the specified type as
/// the root implementation of all generated traits.
///
/// For more information see the documentation of `wit_bindgen::generate!`.
///
/// ```rust
/// # macro_rules! export{ ($($t:tt)*) => (); }
/// # trait Guest {}
/// struct MyType;
///
/// impl Guest for MyType {
///     // ...
/// }
///
/// export!(MyType);
/// ```
#[allow(unused_macros)]
#[doc(hidden)]
macro_rules! __export_plugin_impl {
    ($ty:ident) => {
        self::export!($ty with_types_in self);
    };
    ($ty:ident with_types_in $($path_to_types_root:tt)*) => {
        $($path_to_types_root)*:: __export_world_plugin_cabi!($ty with_types_in
        $($path_to_types_root)*);
    };
}
#[doc(inline)]
pub(crate) use __export_plugin_impl as export;
#[cfg(target_arch = "wasm32")]
#[unsafe(
    link_section = "component-type:wit-bindgen:0.63.0:berth:plugin@0.1.0:plugin:encoded world"
)]
#[doc(hidden)]
#[allow(clippy::octal_escapes)]
pub static __WIT_BINDGEN_COMPONENT_TYPE: [u8; 398] = *b"\
\0asm\x0d\0\x01\0\0\x19\x16wit-component-encoding\x04\0\x07\x91\x02\x01A\x02\x01\
A\x04\x01B\x0c\x01r\x03\x02idw\x05labels\x06active\x7f\x04\0\x0eworkspace-info\x03\
\0\0\x01r\x04\x02idw\x0cworkspace-idw\x05labels\x06active\x7f\x04\0\x08tab-info\x03\
\0\x02\x01p\x01\x01@\0\0\x04\x04\0\x0eget-workspaces\x01\x05\x01p\x03\x01@\0\0\x06\
\x04\0\x08get-tabs\x01\x07\x01@\x02\x02idw\x05labels\x01\0\x04\0\x13set-workspac\
e-label\x01\x08\x03\0\x17berth:plugin/host@0.1.0\x05\0\x01@\0\x01\0\x04\0\x10on-\
layout-change\x01\x01\x04\0\x19berth:plugin/plugin@0.1.0\x04\0\x0b\x0c\x01\0\x06\
plugin\x03\0\0\0G\x09producers\x01\x0cprocessed-by\x02\x0dwit-component\x070.262\
.0\x10wit-bindgen-rust\x060.63.0";
#[inline(never)]
#[doc(hidden)]
pub fn __link_custom_section_describing_imports() {
    wit_bindgen::rt::maybe_link_cabi_realloc();
}
