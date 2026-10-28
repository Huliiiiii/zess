use std::{
    cell::{Ref, RefCell},
    rc::Rc,
};

use string_interner::{StringInterner, backend::StringBackend, symbol::SymbolU32};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BufferKindId(SymbolU32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModeId(SymbolU32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyScopeId(SymbolU32);

/// Session-owned names survive interpreter replacement. Borrows end before
/// calling plugin code, which may register more names on the same thread.
#[derive(Clone)]
pub struct IdentityRegistry(Rc<RefCell<StringInterner<StringBackend>>>);

impl Default for IdentityRegistry {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(StringInterner::new())))
    }
}

impl IdentityRegistry {
    pub fn intern_buffer_kind(&self, name: &str) -> BufferKindId {
        BufferKindId(self.0.borrow_mut().get_or_intern(name))
    }

    pub fn buffer_kind(&self, name: &str) -> Option<BufferKindId> {
        self.0.borrow().get(name).map(BufferKindId)
    }

    pub fn buffer_kind_name(&self, id: BufferKindId) -> Ref<'_, str> {
        Ref::map(self.0.borrow(), |names| names.resolve(id.0).unwrap())
    }

    pub fn intern_mode(&self, name: &str) -> ModeId {
        ModeId(self.0.borrow_mut().get_or_intern(name))
    }

    pub fn mode(&self, name: &str) -> Option<ModeId> {
        self.0.borrow().get(name).map(ModeId)
    }

    pub fn mode_name(&self, id: ModeId) -> Ref<'_, str> {
        Ref::map(self.0.borrow(), |names| names.resolve(id.0).unwrap())
    }

    pub fn intern_scope(&self, name: &str) -> KeyScopeId {
        KeyScopeId(self.0.borrow_mut().get_or_intern(name))
    }

    pub fn scope(&self, name: &str) -> Option<KeyScopeId> {
        self.0.borrow().get(name).map(KeyScopeId)
    }

    pub fn scope_name(&self, id: KeyScopeId) -> Ref<'_, str> {
        Ref::map(self.0.borrow(), |names| names.resolve(id.0).unwrap())
    }
}
