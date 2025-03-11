use std::sync::{Arc, Mutex, Weak};
use pyo3::exceptions::PyException;
use crate::shared_types::ObservationId;
use crate::y_array::YArray;
use crate::y_map::YMap;
use crate::y_text::YText;
use crate::y_transaction::YTransaction;
use crate::y_transaction::YTransactionInner;
use crate::y_xml::YXmlFragment;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::types::PyTuple;
use yrs::updates::encoder::Encode;
use yrs::Doc;
use yrs::OffsetKind;
use yrs::Options;
use yrs::Transact;
use yrs::TransactionCleanupEvent;
use yrs::TransactionMut;

pub trait WithDoc<T> {
    fn with_doc(self, doc: Arc<Mutex<YDocInner>>) -> T;
}
pub trait WithTransaction {
    fn get_doc(&self) -> Arc<Mutex<YDocInner>>;

    fn with_transaction<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&YTransactionInner) -> R,
    {
        let txn = self.get_transaction();
        let mut txn = txn.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        }).unwrap();
        f(&mut txn)
    }

    fn get_transaction(&self) -> Arc<Mutex<YTransactionInner>> {
        let binding = self.get_doc();
        let mut doc = binding.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        }).unwrap();
        let txn = doc.begin_transaction();
        txn
    }
}

pub struct YDocInner {
    doc: Doc,
    txn: Option<Weak<Mutex<YTransactionInner>>>,
}

impl YDocInner {
    pub fn has_transaction(&self) -> bool {
        if let Some(weak_txn) = &self.txn {
            if let Some(txn) = weak_txn.upgrade() {
                let guard = txn.lock().map_err(|e| {
                    PyException::new_err(format!("Mutex lock error: {:?}", e))
                }).unwrap();
                return !guard.committed;
            }
        }
        false
    }

    pub fn begin_transaction(&mut self) -> Arc<Mutex<YTransactionInner>> {
        // Check if we think we still have a transaction
        if let Some(weak_txn) = &self.txn {
            // And if it's actually around
            if let Some(txn) = weak_txn.upgrade() {
                let guard = txn.lock().map_err(|e| {
                    PyException::new_err(format!("Mutex lock error: {:?}", e))
                }).unwrap();
                if !guard.committed {
                    drop(guard);
                    return txn;
                }
            }
        }
        // HACK: get rid of lifetime
        let txn = unsafe {
            std::mem::transmute::<TransactionMut, TransactionMut<'static>>(self.doc.transact_mut())
        };
        let txn = YTransactionInner::new(txn);
        let txn = Arc::new(Mutex::new(txn));
        self.txn = Some(Arc::downgrade(&txn));
        txn
    }

    pub fn commit_transaction(&mut self) {
        if let Some(weak_txn) = &self.txn {
            if let Some(txn) = weak_txn.upgrade() {
                let mut txn = txn.lock().map_err(|e| {
                    PyException::new_err(format!("Mutex lock error: {:?}", e))
                }).unwrap();
                txn.commit();
            }
        }
        self.txn = None;
    }

    pub fn transact_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut YTransactionInner) -> R,
    {
        // HACK: get rid of lifetime
        let txn = unsafe {
            std::mem::transmute::<TransactionMut, TransactionMut<'static>>(self.doc.transact_mut())
        };
        let mut txn = YTransactionInner::new(txn);
        f(&mut txn)
    }
}

/// A Ypy document type. Documents are most important units of collaborative resources management.
/// All shared collections live within a scope of their corresponding documents. All updates are
/// generated on per document basis (rather than individual shared type). All operations on shared
/// collections happen via [YTransaction], which lifetime is also bound to a document.
///
/// Document manages so called root types, which are top-level shared types definitions (as opposed
/// to recursively nested types).
///
/// A basic workflow sample:
///
/// ```python
/// from y_py import YDoc
///
/// doc = YDoc()
/// with doc.begin_transaction() as txn:
///     text = txn.get_text('name')
///     text.push(txn, 'hello world')
///     output = text.to_string(txn)
///     print(output)
/// ```
#[pyclass(unsendable, subclass)]
pub struct YDoc(Arc<Mutex<YDocInner>>);

impl YDoc {
    pub fn guard_store(&self) -> PyResult<()> {
        let guard = self.0.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        })?;
        if guard.has_transaction() {
            return Err(pyo3::exceptions::PyAssertionError::new_err(
                "Transaction already started!",
            ));
        }
        Ok(())
    }
}

#[pymethods]
impl YDoc {
    /// Creates a new Ypy document. If `client_id` parameter was passed it will be used as this
    /// document globally unique identifier (it's up to caller to ensure that requirement).
    /// Otherwise it will be assigned a randomly generated number.
    #[new]
    pub fn new(
        client_id: Option<u64>,
        offset_kind: Option<String>,
        skip_gc: Option<bool>,
    ) -> PyResult<Self> {
        let mut options = Options::default();
        if let Some(client_id) = client_id {
            options.client_id = client_id;
        }

        if let Some(raw_offset) = offset_kind {
            let clean_offset = raw_offset.to_lowercase().replace('-', "");
            let offset = match clean_offset.as_str() {
                "utf8" => Ok(OffsetKind::Bytes),
                "utf16" => Ok(OffsetKind::Utf16),
                _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "'{}' is not a valid offset kind (utf8 or utf16).",
                    clean_offset
                ))),
            }?;
            options.offset_kind = offset;
        }

        if let Some(skip_gc) = skip_gc {
            options.skip_gc = skip_gc;
        }

        let inner = YDocInner {
            doc: Doc::with_options(options),
            txn: None,
        };

        Ok(YDoc(Arc::new(Mutex::new(inner))))
    }

    /// Gets globally unique identifier of this `YDoc` instance.
    #[getter]
    pub fn client_id(&self) -> u64 {
        let guard = self.0.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        }).unwrap();
        guard.doc.client_id()
    }

    /// Returns a new transaction for this document. Ypy shared data types execute their
    /// operations in a context of a given transaction. Each document can have only one active
    /// transaction at the time - subsequent attempts will cause exception to be thrown.
    ///
    /// Transactions started with `doc.begin_transaction` can be released by deleting the transaction object
    /// method.
    ///
    /// Example:
    ///
    /// ```python
    /// from y_py import YDoc
    /// doc = YDoc()
    /// text = doc.get_text('name')
    /// with doc.begin_transaction() as txn:
    ///     text.insert(txn, 0, 'hello world')
    /// ```
    pub fn begin_transaction(&self) -> YTransaction {
        let mut guard = self.0.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        }).unwrap();
        YTransaction::new(guard.begin_transaction())
    }

    pub fn transact(&mut self, callback: PyObject) -> PyResult<PyObject> {
        let mut guard = self.0.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        })?;
        let txn = YTransaction::new(guard.begin_transaction());
        let result = Python::with_gil(|py| {
            let args = PyTuple::new(py, vec![txn.into_py(py)]);
            callback.call(py, args, None)
        });
        // Make transaction commit after callback returns
        let mut doc = self.0.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        })?;
        doc.commit_transaction();
        result
    }

    /// Returns a `YMap` shared data type, that's accessible for subsequent accesses using given
    /// `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YMap` instance.
    pub fn get_map(&mut self, name: &str) -> PyResult<YMap> {
        self.guard_store()?;
        let guard = self.0.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        })?;
        Ok(guard
            .doc
            .get_or_insert_map(name)
            .with_doc(self.0.clone()))
    }

    /// Returns a `YXmlFragment` shared data type, that's accessible for subsequent accesses using
    /// given `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YXmlFragment` instance.
    pub fn get_xml_fragment(&mut self, name: &str) -> PyResult<YXmlFragment> {
        self.guard_store()?;
        let guard = self.0.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        })?;
        Ok(guard
            .doc
            .get_or_insert_xml_fragment(name)
            .with_doc(self.0.clone()))
    }

    /// Returns a `YArray` shared data type, that's accessible for subsequent accesses using given
    /// `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YArray` instance.
    pub fn get_array(&mut self, name: &str) -> PyResult<YArray> {
        self.guard_store()?;
        let guard = self.0.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        })?;
        Ok(guard
            .doc
            .get_or_insert_array(name)
            .with_doc(self.0.clone()))
    }

    /// Returns a `YText` shared data type, that's accessible for subsequent accesses using given
    /// `name`.
    ///
    /// If there was no instance with this name before, it will be created and then returned.
    ///
    /// If there was an instance with this name, but it was of different type, it will be projected
    /// onto `YText` instance.
    pub fn get_text(&mut self, name: &str) -> PyResult<YText> {
        self.guard_store()?;
        let guard = self.0.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        })?;
        Ok(guard
            .doc
            .get_or_insert_text(name)
            .with_doc(self.0.clone()))
    }

    /// Subscribes a callback to a `YDoc` lifecycle event.
    pub fn observe_after_transaction(&mut self, callback: PyObject) -> ObservationId {
        let guard = self.0.lock().map_err(|e| {
            PyException::new_err(format!("Mutex lock error: {:?}", e))
        }).unwrap();
        let subscription = guard
            .doc
            .observe_transaction_cleanup(move |txn, event| {
                Python::with_gil(|py| {
                    let event = AfterTransactionEvent::new(event, txn);
                    if let Err(err) = callback.call1(py, (event,)) {
                        err.restore(py)
                    }
                })
            })
            .unwrap();
        ObservationId(subscription)
    }
}

/// Encodes a state vector of a given Ypy document into its binary representation using yrs v1
/// encoding. State vector is a compact representation of updates performed on a given document and
/// can be used by `encode_state_as_update` on remote peer to generate a delta update payload to
/// synchronize changes between peers.
///
/// Example:
///
/// ```python
/// from y_py import YDoc, encode_state_vector, encode_state_as_update, apply_update from y_py
///
/// # document on machine A
/// local_doc = YDoc()
/// local_sv = encode_state_vector(local_doc)
///
/// # document on machine B
/// remote_doc = YDoc()
/// remote_delta = encode_state_as_update(remote_doc, local_sv)
///
/// apply_update(local_doc, remote_delta)
/// ```
#[pyfunction]
pub fn encode_state_vector(doc: &mut YDoc) -> PyObject {
    let mut guard = doc.0.lock().map_err(|e| {
        PyException::new_err(format!("Mutex lock error: {:?}", e))
    }).unwrap();
    let txn = guard.begin_transaction();
    let txn = YTransaction::new(txn);
    txn.state_vector_v1()
}

/// Encodes all updates that have happened since a given version `vector` into a compact delta
/// representation using yrs v1 encoding. If `vector` parameter has not been provided, generated
/// delta payload will contain all changes of a current Ypy document, working effectively as its
/// state snapshot.
///
/// Example:
///
/// ```python
/// from y_py import YDoc, encode_state_vector, encode_state_as_update, apply_update
///
/// # document on machine A
/// local_doc = YDoc()
/// local_sv = encode_state_vector(local_doc)
///
/// # document on machine B
/// remote_doc = YDoc()
/// remote_delta = encode_state_as_update(remote_doc, local_sv)
///
/// apply_update(local_doc, remote_delta)
/// ```
#[pyfunction]
pub fn encode_state_as_update(doc: &mut YDoc, vector: Option<Vec<u8>>) -> PyResult<PyObject> {
    let mut guard = doc.0.lock().map_err(|e| {
        PyException::new_err(format!("Mutex lock error: {:?}", e))
    })?;
    let txn = guard.begin_transaction();
    YTransaction::new(txn).diff_v1(vector)
}

/// Applies delta update generated by the remote document replica to a current document. This
/// method assumes that a payload maintains yrs v1 encoding format.
///
/// Example:
///
/// ```python
/// from y_py import YDoc, encode_state_vector, encode_state_as_update, apply_update
///
/// # document on machine A
/// local_doc = YDoc()
/// local_sv = encode_state_vector(local_doc)
///
/// # document on machine B
/// remote_doc = YDoc()
/// remote_delta = encode_state_as_update(remote_doc, local_sv)
///
/// apply_update(local_doc, remote_delta)
/// ```
#[pyfunction]
pub fn apply_update(doc: &mut YDoc, diff: Vec<u8>) -> PyResult<()> {
    let mut guard = doc.0.lock().map_err(|e| {
        PyException::new_err(format!("Mutex lock error: {:?}", e))
    })?;
    let txn = guard.begin_transaction();
    YTransaction::new(txn).apply_v1(diff)?;

    Ok(())
}

#[pyclass(unsendable)]
pub struct AfterTransactionEvent {
    before_state: PyObject,
    after_state: PyObject,
    delete_set: PyObject,
    update: PyObject,
}

impl AfterTransactionEvent {
    fn new(event: &TransactionCleanupEvent, txn: &TransactionMut) -> Self {
        // Convert all event data into Python objects eagerly, so that we don't have to hold
        // on to the transaction.
        let before_state = event.before_state.encode_v1();
        let before_state: PyObject = Python::with_gil(|py| PyBytes::new(py, &before_state).into());
        let after_state = event.after_state.encode_v1();
        let after_state: PyObject = Python::with_gil(|py| PyBytes::new(py, &after_state).into());
        let delete_set = event.delete_set.encode_v1();
        let delete_set: PyObject = Python::with_gil(|py| PyBytes::new(py, &delete_set).into());
        let update = txn.encode_update_v1();
        let update = Python::with_gil(|py| PyBytes::new(py, &update).into());
        AfterTransactionEvent {
            before_state,
            after_state,
            delete_set,
            update,
        }
    }
}

#[pymethods]
impl AfterTransactionEvent {
    /// Returns a current shared type instance, that current event changes refer to.
    #[getter]
    pub fn before_state(&mut self) -> PyObject {
        self.before_state.clone()
    }

    #[getter]
    pub fn after_state(&mut self) -> PyObject {
        self.after_state.clone()
    }

    #[getter]
    pub fn delete_set(&mut self) -> PyObject {
        self.delete_set.clone()
    }

    pub fn get_update(&self) -> PyObject {
        self.update.clone()
    }
}
