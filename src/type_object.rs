// Copyright (c) 2017-present PyO3 Project and Contributors
//! Python type object information

use crate::instance::Py;
use crate::pyclass::{initialize_type_object, PyClass};
use crate::pyclass_init::PyObjectInit;
use crate::types::{PyAny, PyType};
use crate::{ffi, AsPyPointer, Python};
use std::cell::UnsafeCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

/// `T: PyObjectLayout<U>` represents that `T` is a concrete representaion of `U` in Python heap.
/// E.g., `PyClassShell` is a concrete representaion of all `pyclass`es, and `ffi::PyObject`
/// is of `PyAny`.
///
/// This trait is intended to be used internally.
pub trait PyObjectLayout<T: PyTypeInfo> {
    const IS_NATIVE_TYPE: bool = true;

    fn get_super_or(&mut self) -> Option<&mut <T::BaseType as PyTypeInfo>::ConcreteLayout> {
        None
    }

    unsafe fn internal_ref_cast(obj: &PyAny) -> &T {
        &*(obj as *const _ as *const T)
    }

    #[allow(clippy::mut_from_ref)]
    unsafe fn internal_mut_cast(obj: &PyAny) -> &mut T {
        &mut *(obj as *const _ as *const T as *mut T)
    }

    unsafe fn py_init(&mut self, _value: T) {}
    unsafe fn py_drop(&mut self, _py: Python) {}
}

/// `T: PyObjectSizedLayout<U>` represents `T` is not a instance of
/// [`PyVarObject`](https://docs.python.org/3.8/c-api/structures.html?highlight=pyvarobject#c.PyVarObject).
/// , in addition that `T` is a concrete representaion of `U`.
///
/// `pyclass`es need this trait for their base class.
pub trait PyObjectSizedLayout<T: PyTypeInfo>: PyObjectLayout<T> + Sized {}

/// Our custom type flags
#[doc(hidden)]
pub mod type_flags {
    /// type object supports python GC
    pub const GC: usize = 1;

    /// Type object supports python weak references
    pub const WEAKREF: usize = 1 << 1;

    /// Type object can be used as the base type of another type
    pub const BASETYPE: usize = 1 << 2;

    /// The instances of this type have a dictionary containing instance variables
    pub const DICT: usize = 1 << 3;

    /// The class declared by #[pyclass(extends=~)]
    pub const EXTENDED: usize = 1 << 4;
}

/// Python type information.
/// All Python native types(e.g., `PyDict`) and `#[pyclass]` structs implement this trait.
///
/// This trait is marked unsafe because:
///  - specifying the incorrect layout can lead to memory errors
///  - the return value of type_object must always point to the same PyTypeObject instance
pub unsafe trait PyTypeInfo: Sized {
    /// Type of objects to store in PyObject struct
    type Type;

    /// Class name
    const NAME: &'static str;

    /// Module name, if any
    const MODULE: Option<&'static str>;

    /// Class doc string
    const DESCRIPTION: &'static str = "\0";

    /// Type flags (ie PY_TYPE_FLAG_GC, PY_TYPE_FLAG_WEAKREF)
    const FLAGS: usize = 0;

    /// Base class
    type BaseType: PyTypeInfo + PyTypeObject;

    /// Layout
    type ConcreteLayout: PyObjectLayout<Self>;

    /// Initializer for layout
    type Initializer: PyObjectInit<Self>;

    /// PyTypeObject instance for this type.
    fn type_object() -> &'static ffi::PyTypeObject;

    /// Check if `*mut ffi::PyObject` is instance of this type
    fn is_instance(object: &PyAny) -> bool {
        unsafe {
            ffi::PyObject_TypeCheck(object.as_ptr(), Self::type_object() as *const _ as _) != 0
        }
    }

    /// Check if `*mut ffi::PyObject` is exact instance of this type
    fn is_exact_instance(object: &PyAny) -> bool {
        unsafe { (*object.as_ptr()).ob_type == Self::type_object() as *const _ as _ }
    }
}

/// Python object types that have a corresponding type object.
///
/// This trait is marked unsafe because not fulfilling the contract for type_object
/// leads to UB.
///
/// See [PyTypeInfo::type_object]
pub unsafe trait PyTypeObject {
    /// Returns the safe abstraction over the type object.
    fn type_object() -> Py<PyType>;
}

unsafe impl<T> PyTypeObject for T
where
    T: PyTypeInfo,
{
    fn type_object() -> Py<PyType> {
        unsafe { Py::from_borrowed_ptr(<Self as PyTypeInfo>::type_object() as *const _ as _) }
    }
}

/// Lazy type object for Exceptions
#[doc(hidden)]
pub struct LazyHeapType {
    value: UnsafeCell<Option<NonNull<ffi::PyTypeObject>>>,
    initialized: AtomicBool,
}

impl LazyHeapType {
    pub const fn new() -> Self {
        LazyHeapType {
            value: UnsafeCell::new(None),
            initialized: AtomicBool::new(false),
        }
    }

    pub fn get_or_init<F>(&self, constructor: F) -> NonNull<ffi::PyTypeObject>
    where
        F: Fn(Python) -> NonNull<ffi::PyTypeObject>,
    {
        if !self
            .initialized
            .compare_and_swap(false, true, Ordering::Acquire)
        {
            // We have to get the GIL before setting the value to the global!!!
            let gil = Python::acquire_gil();
            unsafe {
                *self.value.get() = Some(constructor(gil.python()));
            }
        }
        unsafe { (*self.value.get()).unwrap() }
    }
}

// This is necessary for making static `LazyHeapType`s
//
// Type objects are shared between threads by the Python interpreter anyway, so it is no worse
// to allow sharing on the Rust side too.
unsafe impl Sync for LazyHeapType {}

/// Lazy type object for PyClass
#[doc(hidden)]
pub struct LazyStaticType {
    value: UnsafeCell<ffi::PyTypeObject>,
    initialized: AtomicBool,
}

impl LazyStaticType {
    pub const fn new() -> Self {
        LazyStaticType {
            value: UnsafeCell::new(ffi::PyTypeObject_INIT),
            initialized: AtomicBool::new(false),
        }
    }

    pub fn get_or_init<T: PyClass>(&self) -> &ffi::PyTypeObject {
        if !self
            .initialized
            .compare_and_swap(false, true, Ordering::Acquire)
        {
            let gil = Python::acquire_gil();
            let py = gil.python();
            initialize_type_object::<T>(py, T::MODULE, unsafe { &mut *self.value.get() })
                .unwrap_or_else(|e| {
                    e.print(py);
                    panic!("An error occurred while initializing class {}", T::NAME)
                });
        }
        unsafe { &*self.value.get() }
    }
}

// This is necessary for making static `LazyStaticType`s
unsafe impl Sync for LazyStaticType {}
