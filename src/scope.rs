use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;

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

tokio::task_local! {
    static TASK_SCOPE: Scope;
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

pub async fn with_scope_async<R>(
    configure_scope: impl FnOnce(&mut Scope),
    future: impl Future<Output = R>,
) -> R {
    let mut scope = current_local_scope().unwrap_or_default();
    configure_scope(&mut scope);
    TASK_SCOPE.scope(scope, future).await
}

fn current_thread_scope() -> Option<Scope> {
    LOCAL_SCOPE_STACK.with(|stack| stack.borrow().last().cloned())
}

fn merge_scopes(mut base: Scope, overlay: Scope) -> Scope {
    for (key, value) in overlay.tags {
        base.tags.insert(key, value);
    }
    for (key, value) in overlay.extras {
        base.extras.insert(key, value);
    }
    if overlay.user.is_some() {
        base.user = overlay.user;
    }
    base
}

pub fn current_local_scope() -> Option<Scope> {
    let task_scope = TASK_SCOPE.try_with(|scope| scope.clone()).ok();
    let thread_scope = current_thread_scope();
    match (task_scope, thread_scope) {
        (None, None) => None,
        (Some(scope), None) | (None, Some(scope)) => Some(scope),
        (Some(task_scope), Some(thread_scope)) => Some(merge_scopes(task_scope, thread_scope)),
    }
}

#[cfg(test)]
mod tests {
    use super::{current_local_scope, with_scope, with_scope_async};

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

    #[tokio::test]
    async fn with_scope_async_preserves_scope_across_awaits() {
        let observed = with_scope_async(|scope| scope.set_tag("requestId", "req_1"), async {
            tokio::task::yield_now().await;
            let scope = current_local_scope().expect("scope should exist");
            let mut snapshot = crate::diagnostics::ScopeSnapshot::default();
            scope.merge_into(&mut snapshot);
            snapshot.tags.get("requestId").cloned()
        })
        .await;

        assert_eq!(observed.as_deref(), Some("req_1"));
        assert!(current_local_scope().is_none());
    }
}
