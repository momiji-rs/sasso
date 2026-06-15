use super::*;

impl<'a> Evaluator<'a> {
    // ---- scopes ------------------------------------------------------

    pub(super) fn lookup(&self, name: &str) -> Option<Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.borrow().get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    /// Capture the current variable and function/mixin scope chains as a
    /// callable's lexical closure (dart `Environment.closure()` — shared
    /// frames, not snapshots).
    pub(super) fn capture_callable(&self, def: &Rc<Callable>) -> Rc<UserCallable> {
        Rc::new(UserCallable {
            def: Rc::clone(def),
            env: self.scopes.clone(),
            env_semi: self.scope_semi_global.clone(),
            env_fns: self.functions.clone(),
            env_mixins: self.mixins.clone(),
        })
    }

    /// Push a new scope. `semi_global` requests semi-global behavior (control
    /// flow), which only takes effect when the current innermost scope is
    /// already semi-global (dart-sass `Environment.scope`). The function and
    /// mixin chains push in lockstep with the variable chain.
    pub(super) fn push_scope(&mut self, semi_global: bool) {
        let effective = semi_global && self.scope_semi_global.last().copied().unwrap_or(false);
        self.scopes.push(new_scope());
        self.scope_semi_global.push(effective);
        self.functions.push(new_fn_scope());
        self.mixins.push(new_fn_scope());
    }

    /// Push a pre-populated, non-semi-global scope (a mixin/function argument
    /// frame).
    pub(super) fn push_scope_frame(&mut self, frame: HashMap<String, Value>) {
        self.scopes.push(std::rc::Rc::new(std::cell::RefCell::new(frame)));
        self.scope_semi_global.push(false);
        self.functions.push(new_fn_scope());
        self.mixins.push(new_fn_scope());
    }

    pub(super) fn pop_scope(&mut self) {
        self.scopes.pop();
        self.scope_semi_global.pop();
        self.functions.pop();
        self.mixins.pop();
    }

    /// Define a user `@function` in the innermost frame (dart
    /// `visitFunctionRule`: always `_functions.length - 1`, no semi-global
    /// special case), so the definition is scoped to the enclosing block.
    /// Keys are dash-normalized like dart's parse-time `identifier(normalize:
    /// true)` — `@function a_b` and `@function a-b` define the SAME name (the
    /// AST keeps the original spelling for plain-CSS fallback and messages).
    pub(super) fn define_function(&mut self, name: &str, c: Rc<UserCallable>) {
        if let Some(frame) = self.functions.last() {
            frame
                .borrow_mut()
                .insert(normalize_arg_name(name).into_owned(), c);
        }
    }

    /// Define a user `@mixin` in the innermost frame (dart `visitMixinRule`).
    pub(super) fn define_mixin(&mut self, name: &str, c: Rc<UserCallable>) {
        if let Some(frame) = self.mixins.last() {
            frame
                .borrow_mut()
                .insert(normalize_arg_name(name).into_owned(), c);
        }
    }

    /// Look up a user `@function` (dash/underscore-insensitively, like dart),
    /// innermost frame first.
    pub(super) fn lookup_function(&self, name: &str) -> Option<Rc<UserCallable>> {
        let key = normalize_arg_name(name);
        for frame in self.functions.iter().rev() {
            if let Some(f) = frame.borrow().get(key.as_ref()) {
                return Some(Rc::clone(f));
            }
        }
        None
    }

    /// Look up a user `@mixin` (dash/underscore-insensitively), innermost
    /// frame first.
    pub(super) fn lookup_mixin(&self, name: &str) -> Option<Rc<UserCallable>> {
        let key = normalize_arg_name(name);
        for frame in self.mixins.iter().rev() {
            if let Some(m) = frame.borrow().get(key.as_ref()) {
                return Some(Rc::clone(m));
            }
        }
        None
    }

    /// Look up a user `@function` dash/underscore-insensitively (for the
    /// `meta` introspection functions), innermost frame first.
    pub(super) fn lookup_function_norm(&self, key: &str) -> Option<Rc<UserCallable>> {
        for frame in self.functions.iter().rev() {
            let frame = frame.borrow();
            if let Some((_, f)) = frame.iter().find(|(k, _)| normalize_arg_name(k) == key) {
                return Some(Rc::clone(f));
            }
        }
        None
    }

    /// Look up a user `@mixin` dash/underscore-insensitively, innermost first.
    pub(super) fn lookup_mixin_norm(&self, key: &str) -> Option<Rc<UserCallable>> {
        for frame in self.mixins.iter().rev() {
            let frame = frame.borrow();
            if let Some((_, m)) = frame.iter().find(|(k, _)| normalize_arg_name(k) == key) {
                return Some(Rc::clone(m));
            }
        }
        None
    }

    /// Assign a non-global variable (dart-sass `Environment.setVariable`). The
    /// value updates the variable at the innermost scope where it already
    /// exists; if it exists only in the global scope and the current scope is
    /// not semi-global, a new local is created instead so a nested rule cannot
    /// silently rewrite a global.
    pub(super) fn assign(&mut self, name: &str, val: Value) {
        if self.scopes.len() == 1 {
            if let Some(g) = self.scopes.first_mut() {
                g.borrow_mut().insert(name.to_string(), val);
            }
            return;
        }
        // Innermost scope index holding the variable (None if undeclared).
        let mut index = None;
        for (i, scope) in self.scopes.iter().enumerate().rev() {
            if scope.borrow().contains_key(name) {
                index = Some(i);
                break;
            }
        }
        let in_semi_global = self.scope_semi_global.last().copied().unwrap_or(false);
        let target = match index {
            Some(0) if !in_semi_global => self.scopes.len() - 1,
            Some(i) => i,
            None => self.scopes.len() - 1,
        };
        if let Some(scope) = self.scopes.get_mut(target) {
            scope.borrow_mut().insert(name.to_string(), val);
        }
    }

    pub(super) fn apply_var(&mut self, v: &VarDecl) -> Result<(), Error> {
        // A namespaced assignment `ns.$name: value` updates the variable in the
        // `@use`d module bound to `ns`.
        if let Some(ns) = &v.namespace {
            return self.assign_module_var(ns, v);
        }
        // A top-level `!default` declaration whose name is exposed by more than
        // one `@use … as *` module can't resolve which global it shadows.
        if v.is_default
            && self.scopes.len() == 1
            && self.lookup(&v.name).is_none()
            && !is_private_member(&v.name)
            && self
                .star_user_modules
                .iter()
                .filter(|m| m.var(&v.name).is_some())
                .count()
                > 1
        {
            return Err(Error::unpositioned(
                "This variable is available from multiple global modules.",
            ));
        }
        // A top-level `!default` variable in a module being evaluated with
        // configuration: the supplied value overrides the default (unless the
        // override itself is `!default` and the variable already has a value).
        // Configuration is keyed by the canonical (dashed) variable name.
        if v.is_default && self.scopes.len() == 1 {
            let key = normalize_var_name(&v.name);
            if let Some((cfg_val, cfg_is_default)) = self.pending_config.get(key.as_ref()).cloned() {
                self.consumed_config.push(key.into_owned());
                let already_set = matches!(self.lookup(&v.name), Some(x) if !matches!(x, Value::Null));
                // A `null` configuration value leaves the `!default` in place;
                // a `@forward ... with ($x !default)` only applies if the module
                // hasn't already defined the variable.
                if !(matches!(cfg_val, Value::Null) || cfg_is_default && already_set) {
                    if let Some(g) = self.scopes.first_mut() {
                        g.borrow_mut().insert(v.name.clone(), cfg_val);
                    }
                    return Ok(());
                }
            }
        }
        // A `!default` assignment whose target already holds a non-null value
        // is a no-op — and crucially, the right-hand side must NOT be evaluated
        // (dart-sass short-circuits a guarded declaration before evaluating its
        // expression). Evaluating it would surface errors from an expression
        // that is never actually used, e.g. Bootstrap's
        // `$x: 1rem + .5em !default` after `$x` was already set to a `rem`.
        if v.is_default {
            if let Some(existing) = self.lookup(&v.name) {
                if !matches!(existing, Value::Null) {
                    return Ok(());
                }
            }
            // Same guard for a name owned by exactly one `@use … as *` module.
            if (self.scopes.len() == 1 || v.is_global) && !is_private_member(&v.name) {
                if let Some(g) = self.scopes.first() {
                    if !g.borrow().contains_key(&v.name) {
                        let mut targets = self.star_user_modules.iter().filter_map(|m| m.var(&v.name));
                        if let Some(existing) = targets.next() {
                            if targets.next().is_none() && !matches!(existing, Value::Null) {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
        let val = self.eval_expr(&v.value)?;
        // A top-level (or nested `!global`) assignment to a name not in the
        // global scope but exposed by exactly one `@use … as *` module updates
        // that module's variable (so the module's own functions/mixins observe
        // the change).
        if (self.scopes.len() == 1 || v.is_global) && !is_private_member(&v.name) {
            if let Some(g) = self.scopes.first() {
                if !g.borrow().contains_key(&v.name) {
                    let targets: Vec<Rc<Module>> = self
                        .star_user_modules
                        .iter()
                        .filter(|m| m.var(&v.name).is_some())
                        .cloned()
                        .collect();
                    if targets.len() == 1 {
                        targets[0].vars.borrow_mut().insert(v.name.clone(), val);
                        return Ok(());
                    }
                }
            }
        }
        if v.is_global {
            if let Some(g) = self.scopes.first_mut() {
                g.borrow_mut().insert(v.name.clone(), val);
            }
        } else {
            self.assign(&v.name, val);
        }
        Ok(())
    }

    /// Assign to a `@use`d module's variable (`ns.$name: value`). The variable
    /// must already exist in the module and be public; `!default` only assigns
    /// when the existing value is null; built-in modules are immutable.
    pub(super) fn assign_module_var(&mut self, ns: &str, v: &VarDecl) -> Result<(), Error> {
        if is_private_member(&v.name) {
            return Err(Error::unpositioned(
                "Private members can't be accessed from outside their modules.",
            ));
        }
        let module = match self.used_user_modules.get(ns).cloned() {
            Some(m) => m,
            None => {
                if self.used_modules.contains_key(ns) {
                    return Err(Error::unpositioned("Cannot modify built-in variable."));
                }
                return Err(Error::unpositioned(format!(
                    "There is no module with the namespace \"{ns}\"."
                )));
            }
        };
        // A forwarded variable writes through to its defining module (under
        // its ORIGINAL name), so the module's own functions see the new value.
        let (target, name) = match module.var_write_origin(&v.name) {
            Some((m, o)) => (m, o),
            None => (Rc::clone(&module), v.name.clone()),
        };
        let exists = target.var(&name).is_some();
        if !exists {
            return Err(Error::unpositioned("Undefined variable."));
        }
        // Short-circuit a `!default` no-op before evaluating the RHS, so an
        // unused guarded expression can't raise an error (matches dart-sass).
        if v.is_default {
            if let Some(existing) = target.var(&name) {
                if !matches!(existing, Value::Null) {
                    return Ok(());
                }
            }
        }
        let val = self.eval_expr(&v.value)?.without_slash();
        target.vars.borrow_mut().insert(name, val);
        Ok(())
    }

    // ---- loop helpers ------------------------------------------------

    /// Set a variable in the innermost scope. A loop pushes its own scope, so a
    /// loop variable bound here lives in the loop's scope and is re-bound each
    /// iteration (dart-sass `setLocalVariable`).
    pub(super) fn set_local(&mut self, name: &str, val: Value) {
        if let Some(sc) = self.scopes.last_mut() {
            sc.borrow_mut().insert(name.to_string(), val);
        }
    }
}
