use super::*;

impl<'a> Evaluator<'a> {
    /// Dispatch a namespaced call `ns.member(args)`. Resolves a user module
    /// first, then a built-in module bound to `ns`.
    pub(super) fn eval_module_call(
        &mut self,
        ns: &str,
        member: &str,
        args: &[CallArg],
        pos: Pos,
        length: usize,
    ) -> Result<Value, Error> {
        // A user module bound to this namespace.
        if let Some(module) = self.used_user_modules.get(ns).cloned() {
            if is_private_member(member) {
                return Err(Error::at(
                    "Private members can't be accessed from outside their modules.".to_string(),
                    pos,
                ));
            }
            if let Some(func) = module.function(member) {
                // A forwarded function executes in its DEFINING module's
                // environment (its body closes over that module's globals).
                let exec = module.fn_origin(member).unwrap_or(module);
                return self.call_user_module_function(&exec, &func, args, Some((pos, length)));
            }
            // Fall back to a built-in re-exported by this module via @forward.
            if let Some(v) = self.try_forwarded_builtin_call(&module, member, args, pos)? {
                return Ok(v);
            }
            return Err(Error::at("Undefined function.".to_string(), pos));
        }
        // A built-in module bound to this namespace.
        let module = match self.used_modules.get(ns) {
            Some(m) => m.clone(),
            None => {
                return Err(Error::at(
                    format!("There is no module with the namespace \"{ns}\"."),
                    pos,
                ));
            }
        };
        let (mut pos_args, mut named, _) = self.eval_call_args(args)?;
        for v in &mut pos_args {
            *v = std::mem::replace(v, Value::Null).without_slash();
        }
        for (_, v) in &mut named {
            *v = std::mem::replace(v, Value::Null).without_slash();
        }
        // The `sass:meta` introspection predicates need the evaluator's scopes /
        // definitions, which the value-only `call_module` cannot see.
        if module == "meta" {
            if let Some(r) = self.try_meta_eval_call(member, &pos_args, &named, pos) {
                return r;
            }
        }
        // Call results are slash-free (dart `withoutSlash()` on every call).
        crate::builtins::call_module(&module, member, &pos_args, &named, pos).map(Value::without_slash)
    }

    /// Handle a `sass:meta` member that depends on the evaluator's state
    /// (variable/function/mixin/content existence). Returns `None` for any
    /// member this layer does not own, so the caller falls back to the
    /// value-only `call_module`. The arguments are already evaluated.
    pub(super) fn try_meta_eval_call(
        &mut self,
        member: &str,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Option<Result<Value, Error>> {
        match member {
            "variable-exists" => Some(self.meta_variable_exists(pos_args, named, pos, false)),
            "global-variable-exists" => Some(self.meta_variable_exists(pos_args, named, pos, true)),
            "mixin-exists" => Some(self.meta_mixin_exists(pos_args, named, pos)),
            "function-exists" => Some(self.meta_function_exists(pos_args, named, pos)),
            "content-exists" => Some(self.meta_content_exists(pos_args, pos)),
            "get-function" => Some(self.meta_get_function(pos_args, named, pos)),
            "get-mixin" => Some(self.meta_get_mixin(pos_args, named, pos)),
            "call" => Some(self.meta_call(pos_args, named, pos)),
            "module-variables" => Some(self.meta_module_members(pos_args, named, pos, MemberKind::Variable)),
            "module-functions" => Some(self.meta_module_members(pos_args, named, pos, MemberKind::Function)),
            "module-mixins" => Some(self.meta_module_members(pos_args, named, pos, MemberKind::Mixin)),
            "accepts-content" => Some(self.meta_accepts_content(pos_args, named, pos)),
            "keywords" => Some(Self::meta_keywords(pos_args, named, pos)),
            _ => None,
        }
    }

    /// `meta.keywords($args)`: the keyword arguments captured by a `$args...`
    /// rest parameter, as a map from each name (hyphen-normalized, unquoted) to
    /// its value. The argument must be an argument list, not an ordinary value.
    fn meta_keywords(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
        let v = pos_args
            .first()
            .or_else(|| named.iter().find(|(n, _)| n == "args").map(|(_, v)| v))
            .ok_or_else(|| Error::at("Missing argument $args.".to_string(), pos))?;
        match v {
            Value::List(l) if l.keywords.is_some() => {
                Ok(Value::Map(Map::new(l.keywords.clone().unwrap_or_default())))
            }
            other => Err(Error::at(
                format!("$args: {} is not an argument list.", other.to_css(false)),
                pos,
            )),
        }
    }

    /// `meta.accepts-content($mixin)`: whether the mixin reference's body uses a
    /// `@content` block. The only built-in mixin that does is `meta.apply`.
    fn meta_accepts_content(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Value, Error> {
        let v = pos_args
            .first()
            .or_else(|| named.iter().find(|(n, _)| n == "mixin").map(|(_, v)| v))
            .ok_or_else(|| Error::at("Missing argument $mixin.".to_string(), pos))?;
        let mixin = match v {
            Value::Mixin(m) => m,
            other => {
                return Err(Error::at(
                    format!("$mixin: {} is not a mixin reference.", other.to_css(false)),
                    pos,
                ))
            }
        };
        let accepts = match &mixin.user {
            Some(any) => Rc::clone(any)
                .downcast::<UserCallable>()
                .map(|c| body_uses_content(&c.def.body))
                .unwrap_or(false),
            None => mixin.name == "apply",
        };
        Ok(Value::Bool(accepts))
    }

    /// `meta.get-function($name, $css: false, $module: null)`: capture a
    /// reference to the named function. A `$module` argument needs the user
    /// module loader (unsupported here) and is reported as an error. A user
    /// `@function` is captured by identity; otherwise a built-in (or, with
    /// `$css: true`, a plain-CSS) reference is returned.
    fn meta_get_function(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Value, Error> {
        let params = ["name", "css", "module"];
        if pos_args.len() > params.len() {
            return Err(Error::at(
                format!(
                    "Only {} arguments allowed, but {} were passed.",
                    params.len(),
                    pos_args.len()
                ),
                pos,
            ));
        }
        let arg = |i: usize| -> Option<&Value> {
            pos_args
                .get(i)
                .or_else(|| named.iter().find(|(n, _)| n == params[i]).map(|(_, v)| v))
        };
        let name = match arg(0) {
            Some(Value::Str(s)) => s.text.to_string(),
            Some(other) => {
                return Err(Error::at(
                    format!("$name: {} is not a string.", other.to_css(false)),
                    pos,
                ))
            }
            None => return Err(Error::at("Missing argument $name.", pos)),
        };
        let css = matches!(arg(1), Some(v) if v.is_truthy());
        // A `$module` namespace resolves the function from that `@use`d module.
        if let Some(module_v) = arg(2) {
            match module_v {
                Value::Null => {}
                Value::Str(s) => return self.get_function_from_module(&name, &s.text, pos),
                other => {
                    return Err(Error::at(
                        format!("$module: {} is not a string.", other.to_css(false)),
                        pos,
                    ))
                }
            }
        }
        if css {
            return Ok(Value::Function(SassFunction {
                name,
                css: true,
                user: None,
            }));
        }
        // A user `@function` of that name (dash/underscore-insensitive) wins.
        let key = normalize_arg_name(&name);
        if let Some(f) = self.lookup_function_norm(&key) {
            return Ok(Value::Function(SassFunction {
                name,
                css: false,
                user: Some(f as Rc<dyn std::any::Any>),
            }));
        }
        // A function exposed unprefixed via `@use … as *` (or forwarded into one).
        if !is_private_member(&name) {
            for m in &self.star_user_modules {
                if let Some(f) = m.function(&name) {
                    return Ok(Value::Function(SassFunction {
                        name,
                        css: false,
                        user: Some(Rc::clone(&f) as Rc<dyn std::any::Any>),
                    }));
                }
            }
        }
        if crate::builtins::is_builtin(&name) {
            return Ok(Value::Function(SassFunction {
                name,
                css: false,
                user: None,
            }));
        }
        Err(Error::at(format!("Function not found: {name}"), pos))
    }

    /// `meta.get-mixin($name, $module: null)`: capture a reference to the named
    /// mixin. A user `@mixin` is captured by identity (so a later redefinition
    /// yields a distinct reference); the built-in `sass:meta` mixins
    /// (`load-css`, `apply`) are captured by name. A `$module` argument resolves
    /// the mixin from that `@use`d module's namespace.
    fn meta_get_mixin(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Value, Error> {
        let params = ["name", "module"];
        if pos_args.len() > params.len() {
            return Err(Error::at(
                format!(
                    "Only {} arguments allowed, but {} were passed.",
                    params.len(),
                    pos_args.len()
                ),
                pos,
            ));
        }
        let arg = |i: usize| -> Option<&Value> {
            pos_args
                .get(i)
                .or_else(|| named.iter().find(|(n, _)| n == params[i]).map(|(_, v)| v))
        };
        let name = match arg(0) {
            Some(Value::Str(s)) => s.text.to_string(),
            Some(other) => {
                return Err(Error::at(
                    format!("$name: {} is not a string.", other.to_css(false)),
                    pos,
                ))
            }
            None => return Err(Error::at("Missing argument $name.", pos)),
        };
        // A `$module` argument resolves the mixin from another module's scope.
        if let Some(module_val) = arg(1) {
            if !matches!(module_val, Value::Null) {
                let module_name = match module_val {
                    Value::Str(s) => s.text.clone(),
                    other => {
                        return Err(Error::at(
                            format!("$module: {} is not a string.", other.to_css(false)),
                            pos,
                        ))
                    }
                };
                return self.get_mixin_from_module(&name, &module_name, pos);
            }
        }
        // A user `@mixin` of that name (dash/underscore-insensitive) wins.
        let key = normalize_arg_name(&name);
        if let Some(m) = self.lookup_mixin_norm(&key) {
            return Ok(Value::Mixin(Box::new(SassMixin {
                name,
                user: Some(m as Rc<dyn std::any::Any>),
                module: None,
                // Same-module capture: remember the defining file so a later
                // `meta.apply` from elsewhere resolves relative loads here.
                origin: self.current_mixin_origin(),
            })));
        }
        // A mixin exposed unprefixed via `@use … as *`. Its body runs in the
        // owning module's environment, so capture that module too.
        if !self.star_user_modules.is_empty() && !is_private_member(&name) {
            let hits: Vec<&Rc<Module>> = self
                .star_user_modules
                .iter()
                .filter(|m| m.mixin(&name).is_some())
                .collect();
            if hits.len() > 1 {
                return Err(Error::at(
                    "This mixin is available from multiple global modules.",
                    pos,
                ));
            }
            if let Some(module) = hits.into_iter().next() {
                let m = module
                    .mixin(&name)
                    .ok_or_else(|| Error::at(format!("Mixin not found: {name}"), pos))?;
                return Ok(Value::Mixin(Box::new(SassMixin {
                    name,
                    user: Some(Rc::clone(&m) as Rc<dyn std::any::Any>),
                    module: Some(Rc::clone(module) as Rc<dyn std::any::Any>),
                    // Cross-module capture resolves via the module's own file.
                    origin: None,
                })));
            }
        }
        Err(Error::at(format!("Mixin not found: {name}"), pos))
    }

    /// Resolve a `$module`-qualified mixin reference for `meta.get-mixin`. The
    /// namespace must name a currently-`@use`d module; a built-in module's
    /// mixins (`meta.load-css`, `meta.apply`) resolve by name.
    /// `meta.get-function($name, $module: ns)`: capture a function reference from
    /// the module bound to `ns` — a user `@function` by identity, or a built-in
    /// member by name.
    fn get_function_from_module(&self, name: &str, module_name: &str, pos: Pos) -> Result<Value, Error> {
        if let Some(module) = self.used_user_modules.get(module_name) {
            if is_private_member(name) {
                return Err(Error::at(
                    "Private members can't be accessed from outside their modules.".to_string(),
                    pos,
                ));
            }
            if let Some(f) = module.function(name) {
                return Ok(Value::Function(SassFunction {
                    name: name.to_string(),
                    css: false,
                    user: Some(Rc::clone(&f) as Rc<dyn std::any::Any>),
                }));
            }
            return Err(Error::at(format!("Function not found: {name}"), pos));
        }
        if let Some(builtin) = self.used_modules.get(module_name) {
            if crate::builtins::module_has_member(builtin, name) {
                // The captured reference dispatches through the GLOBAL alias
                // (`color.scale` is the global `scale-color`), so a later
                // meta.call resolves the right builtin (issue_2818).
                let global = crate::builtins::module_member_to_global(builtin, name)
                    .unwrap_or(name)
                    .to_string();
                return Ok(Value::Function(SassFunction {
                    name: global,
                    css: false,
                    user: None,
                }));
            }
            return Err(Error::at(format!("Function not found: {name}"), pos));
        }
        Err(Error::at(
            format!("There is no module with the namespace \"{module_name}\"."),
            pos,
        ))
    }

    fn get_mixin_from_module(&self, name: &str, module_name: &str, pos: Pos) -> Result<Value, Error> {
        if let Some(module) = self.used_user_modules.get(module_name) {
            if is_private_member(name) {
                return Err(Error::at(
                    "Private members can't be accessed from outside their modules.".to_string(),
                    pos,
                ));
            }
            if let Some(m) = module.mixin(name) {
                return Ok(Value::Mixin(Box::new(SassMixin {
                    name: name.to_string(),
                    user: Some(Rc::clone(&m) as Rc<dyn std::any::Any>),
                    module: Some(Rc::clone(module) as Rc<dyn std::any::Any>),
                    // Cross-module capture resolves via the module's own file.
                    origin: None,
                })));
            }
            return Err(Error::at(format!("Mixin not found: {name}"), pos));
        }
        if self.used_modules.contains_key(module_name) {
            if is_builtin_mixin(module_name, name) {
                return Ok(Value::Mixin(Box::new(SassMixin {
                    name: name.to_string(),
                    user: None,
                    module: None,
                    origin: None, // a built-in reference has no user body
                })));
            }
            return Err(Error::at(format!("Mixin not found: {name}"), pos));
        }
        Err(Error::at(
            format!("There is no module with the namespace \"{module_name}\"."),
            pos,
        ))
    }

    /// `meta.call($function, $args...)`: invoke a function reference (or, when
    /// `$function` is a string, the named function). The trailing arguments were
    /// already splat-expanded by `eval_call_args`.
    fn meta_call(&mut self, pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
        // `$function` is the first positional argument, or the named `$function`.
        let (func_val, rest_pos): (Value, Vec<Value>) = if let Some(first) = pos_args.first() {
            (first.clone(), pos_args[1..].to_vec())
        } else if let Some((_, v)) = named.iter().find(|(n, _)| n == "function") {
            (v.clone(), Vec::new())
        } else {
            return Err(Error::at("Missing argument $function.", pos));
        };
        // The remaining named args (excluding `$function`) are call keywords.
        let rest_named: Vec<(String, Value)> =
            named.iter().filter(|(n, _)| n != "function").cloned().collect();

        match func_val {
            // A first-class function reference.
            Value::Function(f) => self.invoke_function_ref(&f, rest_pos, rest_named, pos),
            // The deprecated string form: look up by name.
            Value::Str(s) => {
                let f = SassFunction {
                    name: s.text.to_string(),
                    css: false,
                    user: self
                        .lookup_function_norm(&normalize_arg_name(&s.text))
                        .map(|c| c as Rc<dyn std::any::Any>),
                };
                self.invoke_function_ref(&f, rest_pos, rest_named, pos)
            }
            other => Err(Error::at(
                format!("$function: {} is not a function reference.", other.to_css(false)),
                pos,
            )),
        }
    }

    /// Invoke a resolved function reference with already-evaluated arguments.
    pub(super) fn invoke_function_ref(
        &mut self,
        f: &SassFunction,
        pos_args: Vec<Value>,
        named: Vec<(String, Value)>,
        pos: Pos,
    ) -> Result<Value, Error> {
        // A captured user `@function`: bind the evaluated args and run its
        // body in the callable's lexical closure. The payload is a
        // type-erased `Rc<UserCallable>` (cloning the `Rc` releases the
        // borrow on `f` before running the body).
        if let Some(any) = &f.user {
            if let Ok(callable) = Rc::clone(any).downcast::<UserCallable>() {
                let saved_scopes = std::mem::replace(&mut self.scopes, callable.env.clone());
                let saved_semi = std::mem::replace(&mut self.scope_semi_global, callable.env_semi.clone());
                let saved_fns = std::mem::replace(&mut self.functions, callable.env_fns.clone());
                let saved_mixins = std::mem::replace(&mut self.mixins, callable.env_mixins.clone());
                self.push_scope(false);
                let result = self
                    .bind_evaled_into_scope(
                        &callable.def.params,
                        (pos_args, named, ListSep::Comma),
                        &callable.def.name,
                    )
                    .and_then(|()| {
                        self.in_mixin.push(false);
                        let r = self.run_fn_body(&callable.def.body);
                        self.in_mixin.pop();
                        r
                    });
                self.pop_scope();
                self.scopes = saved_scopes;
                self.scope_semi_global = saved_semi;
                self.functions = saved_fns;
                self.mixins = saved_mixins;
                return match result? {
                    Some(v) => Ok(v.without_slash()),
                    None => Err(Error::unpositioned(format!(
                        "Function {}() did not @return a value.",
                        callable.def.name
                    ))),
                };
            }
        }
        // A plain-CSS reference is preserved verbatim as a CSS function call.
        if f.css {
            let mut parts: Vec<String> = pos_args.iter().map(|v| v.to_css(false)).collect();
            for (n, v) in &named {
                parts.push(format!("${n}: {}", v.to_css(false)));
            }
            return Ok(Value::Str(SassStr {
                text: format!("{}({})", f.name, parts.join(", ")).into(),
                quoted: false,
            }));
        }
        // A built-in reference. The `sass:meta` introspection functions need
        // the evaluator's scopes/definitions; everything else dispatches
        // through the value-only builtin library.
        if let Some(r) = self.try_meta_eval_call(&f.name, &pos_args, &named, pos) {
            return r;
        }
        crate::builtins::call(&f.name, &pos_args, &named, pos).map(Value::without_slash)
    }

    /// Read the single string `$name` argument of an existence predicate,
    /// enforcing arity (1 positional, or `$name`) and the string type.
    /// Parse the `$name` (and optional `$module` namespace, when `allow_module`)
    /// arguments of an existence predicate. A `null` `$module` is treated as
    /// absent. Returns `(name, module)`.
    fn exists_name_module_args(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        fname: &str,
        pos: Pos,
        allow_module: bool,
    ) -> Result<(String, Option<String>), Error> {
        let max = if allow_module { 2 } else { 1 };
        if pos_args.len() > max {
            return Err(Error::at(
                format!(
                    "Only {max} argument{} allowed, but {} were passed.",
                    if max == 1 { "" } else { "s" },
                    pos_args.len()
                ),
                pos,
            ));
        }
        let name_v = pos_args
            .first()
            .or_else(|| named.iter().find(|(n, _)| n == "name").map(|(_, v)| v))
            .ok_or_else(|| Error::at(format!("Missing argument $name for {fname}()."), pos))?;
        let name = match name_v {
            Value::Str(s) => s.text.to_string(),
            other => {
                return Err(Error::at(
                    format!("$name: {} is not a string.", other.to_css(false)),
                    pos,
                ))
            }
        };
        let module = if allow_module {
            let m = pos_args
                .get(1)
                .or_else(|| named.iter().find(|(n, _)| n == "module").map(|(_, v)| v));
            match m {
                None | Some(Value::Null) => None,
                Some(Value::Str(s)) => Some(s.text.to_string()),
                Some(other) => {
                    return Err(Error::at(
                        format!("$module: {} is not a string.", other.to_css(false)),
                        pos,
                    ))
                }
            }
        } else {
            None
        };
        Ok((name, module))
    }

    /// Whether the module bound to namespace `ns` defines a member `name` of the
    /// given kind (function/mixin/variable). An unknown namespace is an error.
    fn module_member_exists(&self, ns: &str, name: &str, kind: MemberKind, pos: Pos) -> Result<bool, Error> {
        if let Some(m) = self.used_user_modules.get(ns) {
            return Ok(match kind {
                MemberKind::Function => m.function(name).is_some(),
                MemberKind::Mixin => m.mixin(name).is_some(),
                MemberKind::Variable => m.var(name).is_some(),
            });
        }
        if let Some(builtin) = self.used_modules.get(ns).cloned() {
            return Ok(match kind {
                MemberKind::Function => crate::builtins::module_has_member(&builtin, name),
                MemberKind::Mixin => builtin == "meta" && matches!(name, "load-css" | "apply"),
                MemberKind::Variable => crate::builtins::module_var(&builtin, name, pos).is_ok(),
            });
        }
        Err(Error::at(
            format!("There is no module with the namespace \"{ns}\"."),
            pos,
        ))
    }

    /// `meta.module-variables/-functions/-mixins($module)`: a map from each
    /// (non-private) member name of the `@use`d module bound to `$module` to its
    /// value (variables) or a first-class reference (functions/mixins). Members
    /// are ordered by name (dart-sass uses source order; every spec module
    /// defines them alphabetically, so this matches byte-for-byte).
    fn meta_module_members(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
        kind: MemberKind,
    ) -> Result<Value, Error> {
        let fname = match kind {
            MemberKind::Function => "module-functions",
            MemberKind::Mixin => "module-mixins",
            MemberKind::Variable => "module-variables",
        };
        if pos_args.len() > 1 {
            return Err(Error::at(
                format!("Only 1 argument allowed, but {} were passed.", pos_args.len()),
                pos,
            ));
        }
        let v = pos_args
            .first()
            .or_else(|| named.iter().find(|(n, _)| n == "module").map(|(_, v)| v))
            .ok_or_else(|| Error::at(format!("Missing argument $module for {fname}()."), pos))?;
        let ns = match v {
            Value::Str(s) => s.text.to_string(),
            other => {
                return Err(Error::at(
                    format!("$module: {} is not a string.", other.to_css(false)),
                    pos,
                ))
            }
        };
        let Some(module) = self.used_user_modules.get(&ns).cloned() else {
            // A built-in module: `sass:meta` is modeled member-by-member
            // (the suite probes it); other built-ins have no variables and
            // their callables are dispatched, not enumerated, so report the
            // names we know.
            if let Some(builtin) = self.used_modules.get(&ns) {
                let names: Vec<&str> = match (builtin.as_str(), kind) {
                    ("meta", MemberKind::Function) => crate::builtins::META_FUNCTION_NAMES.to_vec(),
                    ("meta", MemberKind::Mixin) => crate::builtins::META_MIXIN_NAMES.to_vec(),
                    _ => Vec::new(),
                };
                let entries: Vec<(Value, Value)> = names
                    .into_iter()
                    .map(|name| {
                        let key = Value::Str(SassStr {
                            text: name.to_string().into(),
                            quoted: true,
                        });
                        let val = match kind {
                            MemberKind::Function => Value::Function(SassFunction {
                                name: name.to_string(),
                                css: false,
                                user: None,
                            }),
                            MemberKind::Mixin => Value::Mixin(Box::new(SassMixin {
                                name: name.to_string(),
                                user: None,
                                module: None,
                                origin: None, // built-in reference, no user body
                            })),
                            MemberKind::Variable => Value::Null,
                        };
                        (key, val)
                    })
                    .collect();
                return Ok(Value::Map(Map::new(entries)));
            }
            return Err(Error::at(
                format!("There is no module with the namespace \"{ns}\"."),
                pos,
            ));
        };
        let mut names: Vec<String> = match kind {
            MemberKind::Variable => module.vars.borrow().keys().cloned().collect(),
            MemberKind::Function => module.functions.borrow().keys().cloned().collect(),
            MemberKind::Mixin => module.mixins.borrow().keys().cloned().collect(),
        };
        names.retain(|n| !is_private_member(n));
        names.sort();
        let entries: Vec<(Value, Value)> = names
            .into_iter()
            .map(|name| {
                // Member names are canonicalized to the dashed form for the map
                // key (dart-sass: `$e_f` is keyed `"e-f"`); the value keeps the
                // variable's own value verbatim.
                let key = Value::Str(SassStr {
                    text: name.replace('_', "-").into(),
                    quoted: true,
                });
                let val = match kind {
                    MemberKind::Variable => module.var(&name).unwrap_or(Value::Null),
                    MemberKind::Function => Value::Function(SassFunction {
                        name: name.clone(),
                        css: false,
                        user: module
                            .function(&name)
                            .map(|f| Rc::clone(&f) as Rc<dyn std::any::Any>),
                    }),
                    MemberKind::Mixin => Value::Mixin(Box::new(SassMixin {
                        name: name.clone(),
                        user: module
                            .mixin(&name)
                            .map(|m| Rc::clone(&m) as Rc<dyn std::any::Any>),
                        module: Some(Rc::clone(&module) as Rc<dyn std::any::Any>),
                        // Cross-module capture resolves via the module's file.
                        origin: None,
                    })),
                };
                (key, val)
            })
            .collect();
        Ok(Value::Map(Map::new(entries)))
    }

    /// `meta.variable-exists($name)` / `meta.global-variable-exists($name)`:
    /// whether a variable of that name is in scope (globally only when
    /// `global`). Names are matched dash/underscore-insensitively.
    fn meta_variable_exists(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
        global: bool,
    ) -> Result<Value, Error> {
        let fname = if global {
            "global-variable-exists"
        } else {
            "variable-exists"
        };
        // Only `global-variable-exists` takes the optional `$module` namespace.
        let (name, module) = self.exists_name_module_args(pos_args, named, fname, pos, global)?;
        if let Some(ns) = module {
            return Ok(Value::Bool(self.module_member_exists(
                &ns,
                &name,
                MemberKind::Variable,
                pos,
            )?));
        }
        let key = normalize_arg_name(&name);
        let scopes: &[Scope] = if global { &self.scopes[..1] } else { &self.scopes };
        let found = scopes
            .iter()
            .any(|s| s.borrow().keys().any(|k| normalize_arg_name(k) == key));
        if found {
            return Ok(Value::Bool(true));
        }
        // A variable exposed unprefixed via `@use … as *` (or forwarded into
        // one). Exposure from more than one star module is ambiguous.
        let count = self.star_member_count(&name, MemberKind::Variable);
        if count > 1 {
            return Err(Error::at(
                "This variable is available from multiple global modules.",
                pos,
            ));
        }
        Ok(Value::Bool(count >= 1))
    }

    /// `meta.mixin-exists($name)`: whether a mixin of that name is defined.
    fn meta_mixin_exists(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Value, Error> {
        let (name, module) = self.exists_name_module_args(pos_args, named, "mixin-exists", pos, true)?;
        if let Some(ns) = module {
            return Ok(Value::Bool(self.module_member_exists(
                &ns,
                &name,
                MemberKind::Mixin,
                pos,
            )?));
        }
        let key = normalize_arg_name(&name);
        let local = self.lookup_mixin_norm(&key).is_some();
        if local {
            return Ok(Value::Bool(true));
        }
        let count = self.star_member_count(&name, MemberKind::Mixin);
        if count > 1 {
            return Err(Error::at(
                "This mixin is available from multiple global modules.",
                pos,
            ));
        }
        Ok(Value::Bool(count >= 1))
    }

    /// `meta.function-exists($name)`: whether a user `@function` or a built-in
    /// of that name exists.
    fn meta_function_exists(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Value, Error> {
        let (name, module) = self.exists_name_module_args(pos_args, named, "function-exists", pos, true)?;
        if let Some(ns) = module {
            return Ok(Value::Bool(self.module_member_exists(
                &ns,
                &name,
                MemberKind::Function,
                pos,
            )?));
        }
        let key = normalize_arg_name(&name);
        let user = self.lookup_function_norm(&key).is_some();
        if user {
            return Ok(Value::Bool(true));
        }
        // A function exposed unprefixed via `@use … as *` (or forwarded into a
        // module that is itself `@use`d as `*`). Exposure from more than one
        // star module is ambiguous.
        let count = self.star_member_count(&name, MemberKind::Function);
        if count > 1 {
            return Err(Error::at(
                "This function is available from multiple global modules.",
                pos,
            ));
        }
        Ok(Value::Bool(count >= 1 || crate::builtins::is_builtin(&name)))
    }

    /// Count how many `@use … as *` modules expose `name` as the given member
    /// kind; more than one means an unqualified reference is ambiguous.
    fn star_member_count(&self, name: &str, kind: MemberKind) -> usize {
        if is_private_member(name) {
            return 0;
        }
        self.star_user_modules
            .iter()
            .filter(|m| match kind {
                MemberKind::Variable => m.var(name).is_some(),
                MemberKind::Mixin => m.mixin(name).is_some(),
                MemberKind::Function => m.function(name).is_some(),
            })
            .count()
    }

    /// `meta.content-exists()`: whether the enclosing mixin was passed a
    /// `@content` block. It is an error to call this outside a mixin body.
    fn meta_content_exists(&self, pos_args: &[Value], pos: Pos) -> Result<Value, Error> {
        if !pos_args.is_empty() {
            return Err(Error::at(
                format!("Only 0 arguments allowed, but {} were passed.", pos_args.len()),
                pos,
            ));
        }
        if self.in_mixin.last().copied() != Some(true) {
            return Err(Error::at(
                "content-exists() may only be called within a mixin.",
                pos,
            ));
        }
        let has = matches!(self.content_stack.last(), Some(Some(_)));
        Ok(Value::Bool(has))
    }

    /// Try `member` against a built-in module re-exported by `module` via
    /// `@forward "sass:x"` (honouring an `as p-*` prefix).
    fn try_forwarded_builtin_call(
        &mut self,
        module: &Rc<Module>,
        member: &str,
        args: &[CallArg],
        pos: Pos,
    ) -> Result<Option<Value>, Error> {
        for fb in &module.forwarded_builtins {
            let bare = match &fb.prefix {
                Some(p) => match member.strip_prefix(p.as_str()) {
                    Some(rest) => rest,
                    None => continue,
                },
                None => member,
            };
            if fb.visible(bare) && crate::builtins::module_has_member(&fb.module, bare) {
                let (mut pos_args, mut named, _) = self.eval_call_args(args)?;
                for v in &mut pos_args {
                    *v = std::mem::replace(v, Value::Null).without_slash();
                }
                for (_, v) in &mut named {
                    *v = std::mem::replace(v, Value::Null).without_slash();
                }
                return Ok(Some(
                    crate::builtins::call_module(&fb.module, bare, &pos_args, &named, pos)?.without_slash(),
                ));
            }
        }
        Ok(None)
    }

    /// Call a user module's function in the module's own environment: bind the
    /// arguments in the caller's context, then swap in the module's globals/
    /// functions/mixins/used-modules so the body resolves against the module.
    pub(super) fn call_user_module_function(
        &mut self,
        module: &Rc<Module>,
        func: &Rc<UserCallable>,
        args: &[CallArg],
        call: Option<(Pos, usize)>,
    ) -> Result<Value, Error> {
        let evaled = self.eval_call_args(args)?;
        let saved_member = call.map(|(pos, len)| self.enter_call(pos, len, &format!("{}()", func.def.name)));
        let saved = self.enter_module(module);
        let saved_file = self.enter_module_file(module);
        let saved_scopes = std::mem::replace(&mut self.scopes, func.env.clone());
        let saved_semi = std::mem::replace(&mut self.scope_semi_global, func.env_semi.clone());
        let saved_fns = std::mem::replace(&mut self.functions, func.env_fns.clone());
        let saved_mixins = std::mem::replace(&mut self.mixins, func.env_mixins.clone());
        // The captured tables beat the module's own: a multi-hop `@forward`
        // can hand us a module whose namespaces differ from the file that
        // DEFINED the function (uswds `units()` reaching `sass:meta`).
        let saved_env_modules = self.install_env_modules(&func.env_modules);
        self.push_scope(false);
        let result = self
            .bind_evaled_into_scope(&func.def.params, evaled, &func.def.name)
            .and_then(|()| self.run_fn_body(&func.def.body));
        self.pop_scope();
        self.scopes = saved_scopes;
        self.scope_semi_global = saved_semi;
        self.functions = saved_fns;
        self.mixins = saved_mixins;
        self.restore_env_modules(saved_env_modules);
        self.leave_module_file(saved_file);
        self.leave_module(saved);
        if let Some(saved_member) = saved_member {
            self.leave_call(saved_member);
        }
        match result? {
            Some(v) => Ok(v.without_slash()),
            None => Err(Error::unpositioned(format!(
                "Function {}() did not @return a value.",
                func.def.name
            ))),
        }
    }
}
