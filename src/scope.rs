use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::diagnostics::{ScopeSnapshot, UserContext};

#[derive(Debug, Clone, Default)]
pub struct Scope {
    tags: BTreeMap<String, String>,
    extras: Map<String, Value>,
    user: Option<UserContext>,
}

impl Scope {
    pub fn set_tag(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.tags.insert(key.into(), value.into());
    }

    pub fn remove_tag(&mut self, key: &str) {
        self.tags.remove(key);
    }

    pub fn set_extra(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.extras.insert(key.into(), value.into());
    }

    pub fn remove_extra(&mut self, key: &str) {
        self.extras.remove(key);
    }

    pub fn set_user(&mut self, user: Option<UserContext>) {
        self.user = user;
    }

    pub fn merge_into(&self, destination: &mut ScopeSnapshot) {
        for (key, value) in &self.tags {
            destination.tags.insert(key.clone(), value.clone());
        }
        for (key, value) in &self.extras {
            destination.extras.insert(key.clone(), value.clone());
        }
        if let Some(user) = &self.user {
            destination.user = Some(user.clone());
        }
    }
}

thread_local! {
    static LOCAL_SCOPE_STACK: RefCell<Vec<Scope>> = const { RefCell::new(Vec::new()) };
}

pub fn with_scope<R>(configure_scope: impl FnOnce(&mut Scope), callback: impl FnOnce() -> R) -> R {
    let scope = LOCAL_SCOPE_STACK.with(|stack| {
        let stack = stack.borrow();
        stack.last().cloned().unwrap_or_default()
    });

    let mut scope = scope;
    configure_scope(&mut scope);

    LOCAL_SCOPE_STACK.with(|stack| {
        stack.borrow_mut().push(scope);
    });

    struct ScopeDropGuard;

    impl Drop for ScopeDropGuard {
        fn drop(&mut self) {
            LOCAL_SCOPE_STACK.with(|stack| {
                stack.borrow_mut().pop();
            });
        }
    }

    let _guard = ScopeDropGuard;
    callback()
}

pub fn current_local_scope() -> Option<Scope> {
    LOCAL_SCOPE_STACK.with(|stack| stack.borrow().last().cloned())
}

#[cfg(test)]
mod tests {
    use super::{current_local_scope, with_scope};

    #[test]
    fn nested_with_scope_restores_previous_scope() {
        with_scope(
            |scope| scope.set_tag("requestId", "outer"),
            || {
                let first = current_local_scope().expect("scope should exist");
                let mut had_outer = false;
                first.merge_into(&mut crate::diagnostics::ScopeSnapshot::default());

                with_scope(
                    |scope| scope.set_tag("requestId", "inner"),
                    || {
                        let second = current_local_scope().expect("nested scope should exist");
                        let mut snapshot = crate::diagnostics::ScopeSnapshot::default();
                        second.merge_into(&mut snapshot);
                        had_outer = snapshot.tags.get("requestId") == Some(&"inner".to_string());
                    },
                );

                assert!(had_outer);
            },
        );

        assert!(current_local_scope().is_none());
    }
}
