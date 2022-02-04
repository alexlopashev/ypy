use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::PyIterProtocol;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use yrs::types::xml::{Attributes, TreeWalker, XmlEvent, XmlTextEvent};
use yrs::types::EntryChange;
use yrs::Subscription;
use yrs::Transaction;
use yrs::Xml;
use yrs::XmlElement;
use yrs::XmlText;

use crate::type_conversions::{path_into_py, ToPython};
use crate::y_transaction::YTransaction;

impl ToPython for Xml {
    fn into_py(self, py: Python) -> PyObject {
        match self {
            Xml::Element(v) => YXmlElement(v).into_py(py),
            Xml::Text(v) => YXmlText(v).into_py(py),
        }
    }
}

impl<'a> ToPython for &EntryChange {
    fn into_py(self, py: Python) -> PyObject {
        let result = PyDict::new(py);
        let action = "action";
        match self {
            EntryChange::Inserted(new) => {
                let new_value = new.clone().into_py(py);
                result.set_item(action, "add").unwrap();
                result.set_item("newValue", new_value).unwrap();
            }
            EntryChange::Updated(old, new) => {
                let old_value = old.clone().into_py(py);
                let new_value = new.clone().into_py(py);
                result.set_item(action, "update").unwrap();
                result.set_item("oldValue", old_value).unwrap();
                result.set_item("newValue", new_value).unwrap();
            }
            EntryChange::Removed(old) => {
                let old_value = old.clone().into_py(py);
                result.set_item(action, "delete").unwrap();
                result.set_item("oldValue", old_value).unwrap();
            }
        }
        result.into()
    }
}

/// XML element data type. It represents an XML node, which can contain key-value attributes
/// (interpreted as strings) as well as other nested XML elements or rich text (represented by
/// `YXmlText` type).
///
/// In terms of conflict resolution, `YXmlElement` uses following rules:
///
/// - Attribute updates use logical last-write-wins principle, meaning the past updates are
///   automatically overridden and discarded by newer ones, while concurrent updates made by
///   different peers are resolved into a single value using document id seniority to establish
///   an order.
/// - Child node insertion uses sequencing rules from other Yrs collections - elements are inserted
///   using interleave-resistant algorithm, where order of concurrent inserts at the same index
///   is established using peer's document id seniority.
#[pyclass(unsendable)]
pub struct YXmlElement(pub XmlElement);

#[pymethods]
impl YXmlElement {
    /// Returns a tag name of this XML node.
    #[getter]
    pub fn name(&self) -> String {
        self.0.tag().to_string()
    }

    /// Returns a number of child XML nodes stored within this `YXMlElement` instance.
    pub fn length(&self, txn: &YTransaction) -> u32 {
        self.0.len(txn)
    }

    /// Inserts a new instance of `YXmlElement` as a child of this XML node and returns it.
    pub fn insert_xml_element(
        &self,
        txn: &mut YTransaction,
        index: u32,
        name: &str,
    ) -> YXmlElement {
        YXmlElement(self.0.insert_elem(txn, index, name))
    }

    /// Inserts a new instance of `YXmlText` as a child of this XML node and returns it.
    pub fn insert_xml_text(&self, txn: &mut YTransaction, index: u32) -> YXmlText {
        YXmlText(self.0.insert_text(txn, index))
    }

    /// Removes a range of children XML nodes from this `YXmlElement` instance,
    /// starting at given `index`.
    pub fn delete(&self, txn: &mut YTransaction, index: u32, length: u32) {
        self.0.remove_range(txn, index, length)
    }

    /// Appends a new instance of `YXmlElement` as the last child of this XML node and returns it.
    pub fn push_xml_element(&self, txn: &mut YTransaction, name: &str) -> YXmlElement {
        YXmlElement(self.0.push_elem_back(txn, name))
    }

    /// Appends a new instance of `YXmlText` as the last child of this XML node and returns it.
    pub fn push_xml_text(&self, txn: &mut YTransaction) -> YXmlText {
        YXmlText(self.0.push_text_back(txn))
    }

    /// Returns a first child of this XML node.
    /// It can be either `YXmlElement`, `YXmlText` or `undefined` if current node has not children.
    pub fn first_child(&self, txn: &YTransaction) -> PyObject {
        Python::with_gil(|py| {
            self.0
                .first_child(txn)
                .map_or(py.None(), |xml| xml.into_py(py))
        })
    }

    /// Returns a next XML sibling node of this XMl node.
    /// It can be either `YXmlElement`, `YXmlText` or `undefined` if current node is a last child of
    /// parent XML node.
    pub fn next_sibling(&self, txn: &YTransaction) -> PyObject {
        Python::with_gil(|py| {
            self.0
                .next_sibling(txn)
                .map_or(py.None(), |xml| xml.into_py(py))
        })
    }

    /// Returns a previous XML sibling node of this XMl node.
    /// It can be either `YXmlElement`, `YXmlText` or `undefined` if current node is a first child
    /// of parent XML node.
    pub fn prev_sibling(&self, txn: &YTransaction) -> PyObject {
        Python::with_gil(|py| self.0.prev_sibling(txn).map_or(py.None(), xml_into_py))
    }

    /// Returns a parent `YXmlElement` node or `undefined` if current node has no parent assigned.
    pub fn parent(&self, txn: &YTransaction) -> Option<YXmlElement> {
        self.0.parent(txn).map(YXmlElement)
    }

    /// Returns a string representation of this XML node.
    pub fn to_string(&self, txn: &YTransaction) -> String {
        self.0.to_string(txn)
    }

    /// Sets a `name` and `value` as new attribute for this XML node. If an attribute with the same
    /// `name` already existed on that node, its value with be overridden with a provided one.
    pub fn set_attribute(&self, txn: &mut YTransaction, name: &str, value: &str) {
        self.0.insert_attribute(txn, name, value)
    }

    /// Returns a value of an attribute given its `name`. If no attribute with such name existed,
    /// `null` will be returned.
    pub fn get_attribute(&self, txn: &YTransaction, name: &str) -> Option<String> {
        self.0.get_attribute(txn, name)
    }

    /// Removes an attribute from this XML node, given its `name`.
    pub fn remove_attribute(&self, txn: &mut YTransaction, name: &str) {
        self.0.remove_attribute(txn, &name);
    }

    /// Returns an iterator that enables to traverse over all attributes of this XML node in
    /// unspecified order.
    pub fn attributes(&self, txn: &YTransaction) -> YXmlAttributes {
        unsafe {
            let this: *const XmlElement = &self.0;
            let tx: *const Transaction = txn.deref() as *const _;
            let static_iter: ManuallyDrop<Attributes<'static>> =
                ManuallyDrop::new((*this).attributes(tx.as_ref().unwrap()));
            YXmlAttributes(static_iter)
        }
    }

    /// Returns an iterator that enables a deep traversal of this XML node - starting from first
    /// child over this XML node successors using depth-first strategy.
    pub fn tree_walker(&self, txn: &YTransaction) -> YXmlTreeWalker {
        unsafe {
            let this: *const XmlElement = &self.0;
            let tx: *const Transaction = txn.deref() as *const _;
            let static_iter: ManuallyDrop<TreeWalker<'static>> =
                ManuallyDrop::new((*this).successors(tx.as_ref().unwrap()));
            YXmlTreeWalker(static_iter)
        }
    }

    /// Subscribes to all operations happening over this instance of `YXmlElement`. All changes are
    /// batched and eventually triggered during transaction commit phase.
    /// Returns an `YXmlObserver` which, when free'd, will unsubscribe current callback.
    pub fn observe(&mut self, f: PyObject) -> YXmlObserver {
        self.0
            .observe(move |txn, e| {
                Python::with_gil(|py| {
                    let event = YXmlEvent::new(e, txn);
                    f.call1(py, (event,)).unwrap();
                })
            })
            .into()
    }
}

/// A shared data type used for collaborative text editing, that can be used in a context of
/// `YXmlElement` nodee. It enables multiple users to add and remove chunks of text in efficient
/// manner. This type is internally represented as a mutable double-linked list of text chunks
/// - an optimization occurs during `YTransaction.commit`, which allows to squash multiple
/// consecutively inserted characters together as a single chunk of text even between transaction
/// boundaries in order to preserve more efficient memory model.
///
/// Just like `YXmlElement`, `YXmlText` can be marked with extra metadata in form of attributes.
///
/// `YXmlText` structure internally uses UTF-8 encoding and its length is described in a number of
/// bytes rather than individual characters (a single UTF-8 code point can consist of many bytes).
///
/// Like all Yrs shared data types, `YXmlText` is resistant to the problem of interleaving (situation
/// when characters inserted one after another may interleave with other peers concurrent inserts
/// after merging all updates together). In case of Yrs conflict resolution is solved by using
/// unique document id to determine correct and consistent ordering.
#[pyclass(unsendable)]
pub struct YXmlText(pub XmlText);

#[pymethods]
impl YXmlText {
    /// Returns length of an underlying string stored in this `YXmlText` instance,
    /// understood as a number of UTF-8 encoded bytes.
    #[getter]
    pub fn length(&self) -> u32 {
        self.0.len()
    }

    /// Inserts a given `chunk` of text into this `YXmlText` instance, starting at a given `index`.
    pub fn insert(&self, txn: &mut YTransaction, index: i32, chunk: &str) {
        self.0.insert(txn, index as u32, chunk)
    }

    /// Appends a given `chunk` of text at the end of `YXmlText` instance.
    pub fn push(&self, txn: &mut YTransaction, chunk: &str) {
        self.0.push(txn, chunk)
    }

    /// Deletes a specified range of of characters, starting at a given `index`.
    /// Both `index` and `length` are counted in terms of a number of UTF-8 character bytes.
    pub fn delete(&self, txn: &mut YTransaction, index: u32, length: u32) {
        self.0.remove_range(txn, index, length)
    }

    /// Returns a next XML sibling node of this XMl node.
    /// It can be either `YXmlElement`, `YXmlText` or `undefined` if current node is a last child of
    /// parent XML node.
    pub fn next_sibling(&self, txn: &YTransaction) -> PyObject {
        if let Some(xml) = self.0.next_sibling(txn) {
            xml_into_py(xml)
        } else {
            Python::with_gil(|py| py.None())
        }
    }

    /// Returns a previous XML sibling node of this XMl node.
    /// It can be either `YXmlElement`, `YXmlText` or `undefined` if current node is a first child
    /// of parent XML node.
    pub fn prev_sibling(&self, txn: &YTransaction) -> PyObject {
        if let Some(xml) = self.0.prev_sibling(txn) {
            xml_into_py(xml)
        } else {
            Python::with_gil(|py| py.None())
        }
    }

    /// Returns a parent `YXmlElement` node or `undefined` if current node has no parent assigned.
    pub fn parent(&self, txn: &YTransaction) -> PyObject {
        if let Some(xml) = self.0.parent(txn) {
            xml_into_py(Xml::Element(xml))
        } else {
            Python::with_gil(|py| py.None())
        }
    }

    /// Returns an underlying string stored in this `YXmlText` instance.
    pub fn to_string(&self, txn: &YTransaction) -> String {
        self.0.to_string(txn)
    }

    /// Sets a `name` and `value` as new attribute for this XML node. If an attribute with the same
    /// `name` already existed on that node, its value with be overridden with a provided one.
    pub fn set_attribute(&self, txn: &mut YTransaction, name: &str, value: &str) {
        self.0.insert_attribute(txn, name, value);
    }

    /// Returns a value of an attribute given its `name`. If no attribute with such name existed,
    /// `null` will be returned.
    pub fn get_attribute(&self, txn: &YTransaction, name: &str) -> Option<String> {
        self.0.get_attribute(txn, name)
    }

    /// Removes an attribute from this XML node, given its `name`.
    pub fn remove_attribute(&self, txn: &mut YTransaction, name: &str) {
        self.0.remove_attribute(txn, name);
    }

    /// Returns an iterator that enables to traverse over all attributes of this XML node in
    /// unspecified order.
    pub fn attributes(&self, txn: &YTransaction) -> YXmlAttributes {
        unsafe {
            let this: *const XmlText = &self.0;
            let tx: *const Transaction = txn.deref() as *const _;
            let static_iter: ManuallyDrop<Attributes<'static>> =
                ManuallyDrop::new((*this).attributes(tx.as_ref().unwrap()));
            YXmlAttributes(static_iter)
        }
    }

    /// Subscribes to all operations happening over this instance of `YXmlText`. All changes are
    /// batched and eventually triggered during transaction commit phase.
    /// Returns an `YXmlObserver` which, when free'd, will unsubscribe current callback.
    pub fn observe(&mut self, f: PyObject) -> YXmlTextObserver {
        self.0
            .observe(move |txn, e| {
                Python::with_gil(|py| {
                    let e = YXmlTextEvent::new(e, txn);
                    f.call1(py, (e,)).unwrap();
                })
            })
            .into()
    }
}

#[pyclass(unsendable)]
pub struct YXmlObserver(Subscription<XmlEvent>);

impl From<Subscription<XmlEvent>> for YXmlObserver {
    fn from(o: Subscription<XmlEvent>) -> Self {
        YXmlObserver(o)
    }
}

#[pyclass(unsendable)]
pub struct YXmlTextObserver(Subscription<XmlTextEvent>);

impl From<Subscription<XmlTextEvent>> for YXmlTextObserver {
    fn from(o: Subscription<XmlTextEvent>) -> Self {
        YXmlTextObserver(o)
    }
}

#[pyclass(unsendable)]
pub struct YXmlAttributes(ManuallyDrop<Attributes<'static>>);

impl Drop for YXmlAttributes {
    fn drop(&mut self) {
        unsafe { ManuallyDrop::drop(&mut self.0) }
    }
}

#[pyproto]
impl PyIterProtocol for YXmlAttributes {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }
    fn __next__(mut slf: PyRefMut<Self>) -> Option<(String, String)> {
        slf.0.next().map(|(attr, val)| (attr.to_string(), val))
    }
}

fn xml_into_py(v: Xml) -> PyObject {
    Python::with_gil(|py| match v {
        Xml::Element(v) => YXmlElement(v).into_py(py),
        Xml::Text(v) => YXmlText(v).into_py(py),
    })
}

#[pyclass(unsendable)]
pub struct YXmlTreeWalker(ManuallyDrop<TreeWalker<'static>>);

impl Drop for YXmlTreeWalker {
    fn drop(&mut self) {
        unsafe { ManuallyDrop::drop(&mut self.0) }
    }
}

#[pymethods]
impl YXmlTreeWalker {
    pub fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }
    pub fn __next__(mut slf: PyRefMut<Self>) -> Option<PyObject> {
        Python::with_gil(|py| {
            slf.0.next().map(|v| match v {
                Xml::Element(el) => YXmlElement(el).into_py(py),
                Xml::Text(text) => YXmlText(text).into_py(py),
            })
        })
    }
}

#[pyclass(unsendable)]
pub struct YXmlEvent {
    inner: *const XmlEvent,
    txn: *const Transaction,
    target: Option<PyObject>,
    delta: Option<PyObject>,
    keys: Option<PyObject>,
}
impl YXmlEvent {
    fn new(event: &XmlEvent, txn: &Transaction) -> Self {
        let inner = event as *const XmlEvent;
        let txn = txn as *const Transaction;
        YXmlEvent {
            inner,
            txn,
            target: None,
            delta: None,
            keys: None,
        }
    }

    fn inner(&self) -> &XmlEvent {
        unsafe { self.inner.as_ref().unwrap() }
    }

    fn txn(&self) -> &Transaction {
        unsafe { self.txn.as_ref().unwrap() }
    }
}

#[pymethods]
impl YXmlEvent {
    /// Returns a current shared type instance, that current event changes refer to.
    #[getter]
    pub fn target(&mut self) -> PyObject {
        if let Some(target) = self.target.as_ref() {
            target.clone()
        } else {
            Python::with_gil(|py| {
                let target = YXmlElement(self.inner().target().clone()).into_py(py);
                self.target = Some(target.clone());
                target
            })
        }
    }

    /// Returns an array of keys and indexes creating a path from root type down to current instance
    /// of shared type (accessible via `target` getter).
    /// TODO extract to function
    pub fn path(&self) -> PyObject {
        path_into_py(self.inner().path(self.txn()))
    }

    /// Returns all changes done upon map component of a current shared data type (which can be
    /// accessed via `target`) within a bounds of corresponding transaction `txn`. These
    /// changes are done in result of operations made on `YMap` data type or attribute changes of
    /// `YXmlElement` and `YXmlText` types.
    #[getter]
    pub fn keys(&mut self) -> PyObject {
        if let Some(keys) = &self.keys {
            keys.clone()
        } else {
            Python::with_gil(|py| {
                let keys = self.inner().keys(self.txn());
                let result = PyDict::new(py);
                for (key, value) in keys.iter() {
                    result.set_item(key.deref(), value.into_py(py)).unwrap();
                }
                let keys = PyObject::from(result);
                self.keys = Some(keys.clone());
                keys
            })
        }
    }

    /// Returns collection of all changes done over an array component of a current shared data
    /// type (which can be accessed via `target` property). These changes are usually done in result
    /// of operations done on `YArray` and `YText`/`XmlText` types, but also whenever `XmlElement`
    /// children nodes list is modified.
    #[getter]
    pub fn delta(&mut self) -> PyObject {
        if let Some(delta) = &self.delta {
            delta.clone()
        } else {
            Python::with_gil(|py| {
                let delta = self
                    .inner()
                    .delta(self.txn())
                    .into_iter()
                    .map(|d| Python::with_gil(|py| d.into_py(py)));
                let result = pyo3::types::PyList::new(py, delta);
                let delta: PyObject = result.into();
                self.delta = Some(delta.clone());
                delta
            })
        }
    }
}

#[pyclass(unsendable)]
pub struct YXmlTextEvent {
    inner: *const XmlTextEvent,
    txn: *const Transaction,
    target: Option<PyObject>,
    delta: Option<PyObject>,
    keys: Option<PyObject>,
}

impl YXmlTextEvent {
    fn new(event: &XmlTextEvent, txn: &Transaction) -> Self {
        let inner = event as *const XmlTextEvent;
        let txn = txn as *const Transaction;
        YXmlTextEvent {
            inner,
            txn,
            target: None,
            delta: None,
            keys: None,
        }
    }

    fn inner(&self) -> &XmlTextEvent {
        unsafe { self.inner.as_ref().unwrap() }
    }

    fn txn(&self) -> &Transaction {
        unsafe { self.txn.as_ref().unwrap() }
    }
}

#[pymethods]
impl YXmlTextEvent {
    /// Returns a current shared type instance, that current event changes refer to.
    #[getter]
    pub fn target(&mut self) -> PyObject {
        if let Some(target) = self.target.as_ref() {
            target.clone()
        } else {
            Python::with_gil(|py| {
                let target = YXmlText(self.inner().target().clone()).into_py(py);
                self.target = Some(target.clone());
                target
            })
        }
    }

    /// Returns a current shared type instance, that current event changes refer to.
    pub fn path(&self) -> PyObject {
        path_into_py(self.inner().path(self.txn()))
    }

    /// Returns all changes done upon map component of a current shared data type (which can be
    /// accessed via `target`) within a bounds of corresponding transaction `txn`. These
    /// changes are done in result of operations made on `YMap` data type or attribute changes of
    /// `YXmlElement` and `YXmlText` types.
    #[getter]
    pub fn keys(&mut self) -> PyObject {
        if let Some(keys) = &self.keys {
            keys.clone()
        } else {
            Python::with_gil(|py| {
                let keys = self.inner().keys(self.txn());
                let result = PyDict::new(py);
                for (key, value) in keys.iter() {
                    result.set_item(key.deref(), value.into_py(py));
                }
                let keys = PyObject::from(result);
                self.keys = Some(keys.clone());
                keys
            })
        }
    }

    /// Returns a list of text changes made over corresponding `YXmlText` collection within
    /// bounds of current transaction. These changes follow a format:
    ///
    /// - { insert: string, attributes: any|undefined }
    /// - { delete: number }
    /// - { retain: number, attributes: any|undefined }
    #[getter]
    pub fn delta(&mut self) -> PyObject {
        if let Some(delta) = &self.delta {
            delta.clone()
        } else {
            Python::with_gil(|py| {
                let delta = self
                    .inner()
                    .delta(self.txn())
                    .into_iter()
                    .map(|d| Python::with_gil(|py| d.clone().into_py(py)));
                let result = pyo3::types::PyList::new(py, delta);
                let delta: PyObject = result.into();
                self.delta = Some(delta.clone());
                delta
            })
        }
    }
}
