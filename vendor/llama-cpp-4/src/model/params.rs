//! A safe wrapper around `llama_model_params`.

use crate::model::params::kv_overrides::KvOverrides;
use std::ffi::{c_char, CStr};
use std::fmt::{Debug, Formatter};
use std::pin::Pin;
use std::ptr::null;

pub mod kv_overrides;

/// A safe wrapper around `llama_model_params`.
#[allow(clippy::module_name_repetitions)]
pub struct LlamaModelParams {
    pub(crate) params: llama_cpp_sys_4::llama_model_params,
    kv_overrides: Vec<llama_cpp_sys_4::llama_model_kv_override>,
    /// Backing storage for `params.tensor_split` (the per-device layer split).
    /// Held here so the raw `*const f32` handed to llama.cpp stays valid for
    /// the whole lifetime of this params object. Empty ⇒ pointer is null.
    tensor_split: Vec<f32>,
    /// Backing storage for `params.devices` (explicit, null-terminated device
    /// list). Held here so the raw pointer handed to llama.cpp stays valid for
    /// the lifetime of this params object. Empty ⇒ pointer is null (llama.cpp
    /// enumerates all registered devices).
    devices: Vec<llama_cpp_sys_4::ggml_backend_dev_t>,
    /// Backing storage for `params.tensor_buft_overrides` — explicit per-tensor
    /// device placement by name regex (the `-ot` mechanism). Held so the raw,
    /// null-terminated array pointer handed to llama.cpp stays valid for the
    /// params' lifetime. Empty ⇒ pointer is null (no overrides).
    buft_overrides: Vec<llama_cpp_sys_4::llama_model_tensor_buft_override>,
    /// Backing storage for the override pattern C-strings; the structs in
    /// `buft_overrides` hold raw `*const c_char` into these, so they must outlive
    /// the load. Kept in lock-step with `buft_overrides`.
    buft_override_patterns: Vec<std::ffi::CString>,
}

impl Debug for LlamaModelParams {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlamaModelParams")
            .field("n_gpu_layers", &self.params.n_gpu_layers)
            .field("main_gpu", &self.params.main_gpu)
            .field("vocab_only", &self.params.vocab_only)
            .field("use_mmap", &self.params.use_mmap)
            .field("use_mlock", &self.params.use_mlock)
            .field("kv_overrides", &"vec of kv_overrides")
            .finish()
    }
}

impl LlamaModelParams {
    /// See [`KvOverrides`]
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use llama_cpp_4::model::params::LlamaModelParams;
    /// let params = Box::pin(LlamaModelParams::default());
    /// let kv_overrides = params.kv_overrides();
    /// let count = kv_overrides.into_iter().count();
    /// assert_eq!(count, 0);
    /// ```
    #[must_use]
    pub fn kv_overrides(&self) -> KvOverrides<'_> {
        KvOverrides::new(self)
    }

    /// Appends a key-value override to the model parameters. It must be pinned as this creates a self-referential struct.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use std::ffi::{CStr, CString};
    /// use std::pin::pin;
    /// # use llama_cpp_4::model::params::LlamaModelParams;
    /// # use llama_cpp_4::model::params::kv_overrides::ParamOverrideValue;
    /// let mut params = pin!(LlamaModelParams::default());
    /// let key = CString::new("key").expect("CString::new failed");
    /// params.as_mut().append_kv_override(&key, ParamOverrideValue::Int(50));
    ///
    /// let kv_overrides = params.kv_overrides().into_iter().collect::<Vec<_>>();
    /// assert_eq!(kv_overrides.len(), 1);
    ///
    /// let (k, v) = &kv_overrides[0];
    /// assert_eq!(v, &ParamOverrideValue::Int(50));
    ///
    /// assert_eq!(k.to_bytes(), b"key", "expected key to be 'key', was {:?}", k);
    /// ```
    #[allow(clippy::missing_panics_doc)] // panics are just to enforce internal invariants, not user errors
    pub fn append_kv_override(
        mut self: Pin<&mut Self>,
        key: &CStr,
        value: kv_overrides::ParamOverrideValue,
    ) {
        let kv_override = self
            .kv_overrides
            .get_mut(0)
            .expect("kv_overrides did not have a next allocated");

        assert_eq!(kv_override.key[0], 0, "last kv_override was not empty");

        // There should be some way to do this without iterating over everything.
        for (i, &c) in key.to_bytes_with_nul().iter().enumerate() {
            kv_override.key[i] = c_char::try_from(c).expect("invalid character in key");
        }

        kv_override.tag = value.tag();
        kv_override.__bindgen_anon_1 = value.value();

        // set to null pointer for panic safety (as push may move the vector, invalidating the pointer)
        self.params.kv_overrides = null();

        // push the next one to ensure we maintain the iterator invariant of ending with a 0
        self.kv_overrides
            .push(llama_cpp_sys_4::llama_model_kv_override {
                key: [0; 128],
                tag: 0,
                __bindgen_anon_1: llama_cpp_sys_4::llama_model_kv_override__bindgen_ty_1 {
                    val_i64: 0,
                },
            });

        // set the pointer to the (potentially) new vector
        self.params.kv_overrides = self.kv_overrides.as_ptr();

        eprintln!("saved ptr: {:?}", self.params.kv_overrides);
    }
}

impl LlamaModelParams {
    /// Get the number of layers to offload to the GPU.
    #[must_use]
    pub fn n_gpu_layers(&self) -> i32 {
        self.params.n_gpu_layers
    }

    /// The GPU that is used for scratch and small tensors
    #[must_use]
    pub fn main_gpu(&self) -> i32 {
        self.params.main_gpu
    }

    /// only load the vocabulary, no weights
    #[must_use]
    pub fn vocab_only(&self) -> bool {
        self.params.vocab_only
    }

    /// use mmap if possible
    #[must_use]
    pub fn use_mmap(&self) -> bool {
        self.params.use_mmap
    }

    /// force system to keep model in RAM
    #[must_use]
    pub fn use_mlock(&self) -> bool {
        self.params.use_mlock
    }

    /// sets the number of gpu layers to offload to the GPU.
    /// ```
    /// # use llama_cpp_4::model::params::LlamaModelParams;
    /// let params = LlamaModelParams::default();
    /// let params = params.with_n_gpu_layers(1);
    /// assert_eq!(params.n_gpu_layers(), 1);
    /// ```
    #[must_use]
    pub fn with_n_gpu_layers(mut self, n_gpu_layers: u32) -> Self {
        // The only way this conversion can fail is if u32 overflows the i32 - in which case we set
        // to MAX
        let n_gpu_layers = i32::try_from(n_gpu_layers).unwrap_or(i32::MAX);
        self.params.n_gpu_layers = n_gpu_layers;
        self
    }

    /// sets the main GPU
    #[must_use]
    pub fn with_main_gpu(mut self, main_gpu: i32) -> Self {
        self.params.main_gpu = main_gpu;
        self
    }

    /// Sets the tensor split: the per-device fraction of the model when
    /// splitting layers across multiple devices. The order matches llama.cpp's
    /// assembled device list (RPC devices first, then local GPUs). An empty
    /// slice clears it, so llama.cpp falls back to its memory-proportional
    /// default.
    ///
    /// The values are copied and retained inside this params object for the
    /// duration of model loading, so the caller does not need to keep the
    /// slice alive.
    #[must_use]
    pub fn with_tensor_split(mut self, split: &[f32]) -> Self {
        self.tensor_split = split.to_vec();
        self.params.tensor_split = if self.tensor_split.is_empty() {
            null()
        } else {
            self.tensor_split.as_ptr()
        };
        self
    }

    /// Sets the explicit device list the model loads across, as a
    /// null-terminated `ggml_backend_dev_t` array. Order matters and must match
    /// `with_tensor_split`'s expectation (RPC devices first, then local GPUs).
    /// An empty slice clears it, so llama.cpp falls back to enumerating all
    /// registered devices. The pointers are copied and retained for the params'
    /// lifetime; the pointed-to ggml devices are process-static, so the caller
    /// need not keep anything else alive.
    #[must_use]
    pub fn with_devices(mut self, devices: &[llama_cpp_sys_4::ggml_backend_dev_t]) -> Self {
        if devices.is_empty() {
            self.devices = Vec::new();
            self.params.devices = std::ptr::null_mut();
            return self;
        }
        self.devices = devices.to_vec();
        self.devices.push(std::ptr::null_mut()); // null terminator
        self.params.devices = self.devices.as_ptr() as *mut llama_cpp_sys_4::ggml_backend_dev_t;
        self
    }

    /// sets `vocab_only`
    #[must_use]
    pub fn with_vocab_only(mut self, vocab_only: bool) -> Self {
        self.params.vocab_only = vocab_only;
        self
    }

    /// sets `use_mlock`
    #[must_use]
    pub fn with_use_mlock(mut self, use_mlock: bool) -> Self {
        self.params.use_mlock = use_mlock;
        self
    }

    /// Sets explicit per-tensor device placement via name-regex overrides — the
    /// `--override-tensor` (`-ot`) mechanism. Each `(pattern, buft)` pins every
    /// tensor whose name matches the regex `pattern` (llama.cpp uses
    /// `std::regex_search`) onto buffer type `buft` (e.g. a specific RPC worker or
    /// local GPU), **bypassing the proportional layer split**. This lets the
    /// caller OWN placement deterministically instead of predicting llama.cpp's
    /// split. An empty slice clears it. The patterns and the override array are
    /// copied + retained for the params' lifetime; the `buft`s are process-static
    /// (owned by ggml's device registry), so the caller need not keep them alive.
    #[must_use]
    pub fn with_tensor_buft_overrides(
        mut self,
        overrides: &[(std::ffi::CString, llama_cpp_sys_4::ggml_backend_buffer_type_t)],
    ) -> Self {
        if overrides.is_empty() {
            self.buft_overrides = Vec::new();
            self.buft_override_patterns = Vec::new();
            self.params.tensor_buft_overrides = null();
            return self;
        }
        // Keep the pattern C-strings alive; the override structs borrow their ptrs.
        self.buft_override_patterns = overrides.iter().map(|(p, _)| p.clone()).collect();
        let mut raw: Vec<llama_cpp_sys_4::llama_model_tensor_buft_override> = self
            .buft_override_patterns
            .iter()
            .zip(overrides.iter())
            .map(
                |(pat, (_, buft))| llama_cpp_sys_4::llama_model_tensor_buft_override {
                    pattern: pat.as_ptr(),
                    buft: *buft,
                },
            )
            .collect();
        // Null-pattern terminator: llama.cpp iterates the array until pattern == NULL.
        raw.push(llama_cpp_sys_4::llama_model_tensor_buft_override {
            pattern: null(),
            buft: std::ptr::null_mut(),
        });
        self.params.tensor_buft_overrides = raw.as_ptr();
        self.buft_overrides = raw;
        self
    }

}

/// Default parameters for `LlamaModel`. (as defined in llama.cpp by `llama_model_default_params`)
/// ```
/// # use llama_cpp_4::model::params::LlamaModelParams;
/// let params = LlamaModelParams::default();
/// #[cfg(not(target_os = "macos"))]
/// assert_eq!(params.n_gpu_layers(), 0, "n_gpu_layers should be 0");
/// #[cfg(target_os = "macos")]
/// assert_eq!(params.n_gpu_layers(), -1, "n_gpu_layers should be -1 (all layers)");
/// assert_eq!(params.main_gpu(), 0, "main_gpu should be 0");
/// assert_eq!(params.vocab_only(), false, "vocab_only should be false");
/// assert_eq!(params.use_mmap(), true, "use_mmap should be true");
/// assert_eq!(params.use_mlock(), false, "use_mlock should be false");
/// ```
impl Default for LlamaModelParams {
    fn default() -> Self {
        let default_params = unsafe { llama_cpp_sys_4::llama_model_default_params() };
        LlamaModelParams {
            params: default_params,
            // push the next one to ensure we maintain the iterator invariant of ending with a 0
            kv_overrides: vec![llama_cpp_sys_4::llama_model_kv_override {
                key: [0; 128],
                tag: 0,
                __bindgen_anon_1: llama_cpp_sys_4::llama_model_kv_override__bindgen_ty_1 {
                    val_i64: 0,
                },
            }],
            tensor_split: Vec::new(),
            devices: Vec::new(),
            buft_overrides: Vec::new(),
            buft_override_patterns: Vec::new(),
        }
    }
}
