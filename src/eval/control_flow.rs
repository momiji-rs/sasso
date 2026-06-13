use super::*;

impl<'a> Evaluator<'a> {
    /// Evaluate a `@for` bound to a [`Number`], preserving its unit (the loop
    /// variable inherits the `from` bound's unit).
    fn eval_for_number(&mut self, e: &Expr) -> Result<Number, Error> {
        match self.eval_expr(e)? {
            Value::Number(n) => Ok(n),
            other => Err(Error::unpositioned(format!(
                "{} is not a number.",
                other.type_name()
            ))),
        }
    }

    /// Resolve a `@for`'s bounds: the integer start, the integer end (the TO
    /// bound converted to the FROM bound's unit), and the loop variable's unit
    /// (taken from FROM). Errors on incompatible units or a non-integer bound,
    /// matching dart-sass.
    pub(super) fn for_bounds(&mut self, from: &Expr, to: &Expr) -> Result<(i64, i64, String), Error> {
        let start = self.eval_for_number(from)?;
        let end = self.eval_for_number(to)?;
        // The loop variable takes FROM's unit; TO is converted to match. A
        // unitless side defers (no conversion); two incompatible real units err.
        let end_value = if start.is_unitless() || end.is_unitless() {
            end.value
        } else {
            match crate::value::convert_factor(end.unit(), start.unit()) {
                Some(f) => end.value * f,
                None => {
                    return Err(Error::unpositioned(format!(
                        "Expected {} to have unit {}.",
                        Value::Number(end.clone()).to_css(false),
                        start.unit(),
                    )))
                }
            }
        };
        // Both bounds must be integers (dart-sass: "<n> is not an int.").
        let to_int = |v: f64, n: Number| -> Result<i64, Error> {
            if (v - v.round()).abs() < 1e-11 {
                Ok(v.round() as i64)
            } else {
                Err(Error::unpositioned(format!(
                    "{} is not an int.",
                    Value::Number(n).to_css(false)
                )))
            }
        };
        let start_i = to_int(start.value, start.clone())?;
        let end_i = to_int(end_value, Number::with_unit(end_value, start.unit().to_string()))?;
        Ok((start_i, end_i, start.unit().to_string()))
    }

    /// The values `@each` iterates: a list yields its items, `null` yields
    /// nothing, and any other value is iterated once.
    pub(super) fn eval_each_items(&mut self, e: &Expr) -> Result<Vec<Value>, Error> {
        match self.eval_expr(e)? {
            Value::List(l) => Ok(l.items.to_vec()),
            // `@each` over a map yields each `key value` pair as a two-element
            // space list, so `@each $k, $v in $map` destructures correctly.
            Value::Map(m) => Ok(m
                .entries
                .as_ref()
                .clone()
                .into_iter()
                .map(|(k, v)| {
                    Value::List(List {
                        items: vec![k, v].into(),
                        sep: ListSep::Space,
                        bracketed: false,
                        keywords: None,
                    })
                })
                .collect()),
            Value::Null => Ok(Vec::new()),
            other => Ok(vec![other]),
        }
    }

    /// Bind `@each` variables to an item, destructuring a list across
    /// multiple variables (missing elements become `null`).
    pub(super) fn bind_each(&mut self, vars: &[String], item: Value) {
        if vars.len() == 1 {
            self.set_local(&vars[0], item);
            return;
        }
        let elems: Vec<Value> = match item {
            Value::List(l) => l.items.to_vec(),
            other => vec![other],
        };
        for (i, v) in vars.iter().enumerate() {
            let val = elems.get(i).cloned().unwrap_or(Value::Null);
            self.set_local(v, val);
        }
    }

    /// Evaluate call arguments and bind them to a parameter list, returning
    /// the call frame: positional args fill params in order, then keyword
    /// args by name, then declared defaults; extra positionals collect into
    /// a `$rest...` parameter or are an error.
    /// Evaluate a call's argument list into separate positional and keyword
    /// vectors, expanding any `...` splat (a list spreads into positional
    /// args, a map into keyword args). Duplicate keyword names (after
    /// hyphen/underscore normalization) are rejected, and a positional arg
    /// after a keyword arg is an error — matching dart-sass.
    pub(super) fn eval_call_args(&mut self, args: &[CallArg]) -> Result<EvaledArgs, Error> {
        // Explicit positional args are gathered first; positionals spread from
        // a `...` splat are appended after them, so `f([1, 2]..., 3)` binds
        // `3` before `1, 2` (matching dart-sass's misplaced-rest behaviour).
        let mut explicit_pos = Vec::new();
        let mut splat_pos = Vec::new();
        let mut keyword: Vec<(String, Value)> = Vec::new();
        let mut seen_named = false;
        // A splatted list's separator survives into the callee's rest arglist
        // (`foo(c d e...)` binds `$zs` as a SPACE-separated arglist).
        let mut rest_sep = ListSep::Comma;
        let push_named = |keyword: &mut Vec<(String, Value)>, name: String, v: Value| -> Result<(), Error> {
            let norm = normalize_arg_name(&name);
            if keyword.iter().any(|(n, _)| normalize_arg_name(n) == norm) {
                return Err(Error::unpositioned("Duplicate argument."));
            }
            keyword.push((name, v));
            Ok(())
        };
        for a in args {
            let v = self.eval_expr(&a.value)?;
            if a.splat {
                // A splat list spreads into positional args; a map spreads
                // into keyword args (string keys only). A single non-list/map
                // value acts as one positional arg; `null` spreads to nothing.
                match v {
                    Value::Map(m) => {
                        for (k, val) in m.entries.as_ref().clone() {
                            let key = match &k {
                                Value::Str(s) => s.text.to_string(),
                                other => {
                                    return Err(Error::unpositioned(format!(
                                        "{} is not a string in $args.",
                                        other.to_css(false)
                                    )))
                                }
                            };
                            push_named(&mut keyword, key, val)?;
                        }
                    }
                    Value::List(l) => {
                        if !matches!(l.sep, ListSep::Undecided) {
                            rest_sep = l.sep;
                        }
                        splat_pos.extend(l.items.to_vec());
                        // An argument-list splat (`$args...`) also forwards its
                        // captured keyword arguments as named arguments.
                        if let Some(kw) = l.keywords {
                            for (k, val) in kw {
                                if let Value::Str(s) = k {
                                    push_named(&mut keyword, s.text.to_string(), val)?;
                                }
                            }
                        }
                    }
                    Value::Null => {}
                    other => splat_pos.push(other),
                }
                continue;
            }
            match &a.name {
                Some(n) => {
                    push_named(&mut keyword, n.clone(), v)?;
                    seen_named = true;
                }
                None => {
                    // A plain positional arg may not follow a keyword arg.
                    if seen_named {
                        return Err(Error::unpositioned(
                            "Positional arguments must come before keyword arguments.",
                        ));
                    }
                    explicit_pos.push(v);
                }
            }
        }
        explicit_pos.extend(splat_pos);
        Ok((explicit_pos, keyword, rest_sep))
    }

    fn bind_args(
        &mut self,
        params: &ParamList,
        args: &[CallArg],
        name: &str,
    ) -> Result<HashMap<String, Value>, Error> {
        let evaled = self.eval_call_args(args)?;
        self.bind_evaled(params, evaled, name)
    }

    /// Bind evaluated arguments into the CURRENT (freshly pushed) scope.
    /// Parameter defaults evaluate inside the callee environment with the
    /// already-bound parameters visible (`@mixin m($a, $b: $a)`), matching
    /// dart's progressive binding.
    pub(super) fn bind_evaled_into_scope(
        &mut self,
        params: &ParamList,
        evaled: EvaledArgs,
        name: &str,
    ) -> Result<(), Error> {
        let (positional, keyword_vec, rest_sep) = evaled;
        let mut keyword: HashMap<String, Value> = HashMap::default();
        let mut keyword_order: Vec<(String, String)> = Vec::new();
        for (n, v) in keyword_vec {
            let norm = normalize_arg_name(&n).into_owned();
            if !keyword.contains_key(&norm) {
                keyword_order.push((norm.clone(), n));
            }
            keyword.insert(norm, v);
        }
        let mut pos_iter = positional.into_iter();
        for param in &params.params {
            let val = if let Some(v) = pos_iter.next() {
                v
            } else if let Some(v) = keyword.remove(normalize_arg_name(&param.name).as_ref()) {
                v
            } else if let Some(def) = &param.default {
                self.eval_expr(def)?
            } else {
                return Err(Error::unpositioned(format!("Missing argument ${}.", param.name)));
            };
            if let Some(sc) = self.scopes.last() {
                sc.borrow_mut().insert(param.name.clone(), val);
            }
        }
        if let Some(rest) = &params.rest {
            let remaining: Vec<Value> = pos_iter.collect();
            let kw: Vec<(Value, Value)> = keyword_order
                .iter()
                .filter_map(|(norm, _)| {
                    keyword.remove(norm).map(|v| {
                        (
                            Value::Str(SassStr {
                                text: norm.clone().into(),
                                quoted: false,
                            }),
                            v,
                        )
                    })
                })
                .collect();
            if let Some(sc) = self.scopes.last() {
                sc.borrow_mut().insert(
                    rest.clone(),
                    Value::List(List {
                        items: remaining.into(),
                        sep: rest_sep,
                        bracketed: false,
                        keywords: Some(kw),
                    }),
                );
            }
        } else if pos_iter.next().is_some() {
            return Err(Error::unpositioned(format!(
                "{name} was passed too many arguments."
            )));
        }
        if params.rest.is_none() && !keyword.is_empty() {
            let leftover: Vec<&str> = keyword_order
                .iter()
                .filter(|(norm, _)| keyword.contains_key(norm))
                .map(|(_, orig)| orig.as_str())
                .collect();
            if let Some((last, init)) = leftover.split_last() {
                let msg = if init.is_empty() {
                    format!("No parameter named ${last}.")
                } else {
                    let head = init
                        .iter()
                        .map(|n| format!("${n}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("No parameters named {head} or ${last}.")
                };
                return Err(Error::unpositioned(msg));
            }
        }
        Ok(())
    }

    /// Bind already-evaluated `(positional, keyword)` arguments into a call
    /// frame. Used by `meta.call`, which has only evaluated values to pass on.
    fn bind_evaled(
        &mut self,
        params: &ParamList,
        evaled: EvaledArgs,
        name: &str,
    ) -> Result<HashMap<String, Value>, Error> {
        let (positional, keyword_vec, rest_sep) = evaled;
        let mut keyword: HashMap<String, Value> = HashMap::default();
        // Track the order and source spelling of keyword names so an
        // "unknown parameter" error can list them as the caller wrote them.
        let mut keyword_order: Vec<(String, String)> = Vec::new();
        for (n, v) in keyword_vec {
            let norm = normalize_arg_name(&n).into_owned();
            if !keyword.contains_key(&norm) {
                keyword_order.push((norm.clone(), n));
            }
            keyword.insert(norm, v);
        }
        let mut frame = HashMap::default();
        let mut pos_iter = positional.into_iter();
        for param in &params.params {
            let val = if let Some(v) = pos_iter.next() {
                v
            } else if let Some(v) = keyword.remove(normalize_arg_name(&param.name).as_ref()) {
                v
            } else if let Some(def) = &param.default {
                self.eval_expr(def)?
            } else {
                return Err(Error::unpositioned(format!("Missing argument ${}.", param.name)));
            };
            frame.insert(param.name.clone(), val);
        }
        if let Some(rest) = &params.rest {
            let remaining: Vec<Value> = pos_iter.collect();
            // Any keyword args left after binding the declared params become the
            // arglist's keywords, in caller order and keyed by their
            // hyphen-normalized name (what `meta.keywords` reports).
            let kw: Vec<(Value, Value)> = keyword_order
                .iter()
                .filter_map(|(norm, _)| {
                    keyword.remove(norm).map(|v| {
                        (
                            Value::Str(SassStr {
                                text: norm.clone().into(),
                                quoted: false,
                            }),
                            v,
                        )
                    })
                })
                .collect();
            frame.insert(
                rest.clone(),
                Value::List(List {
                    items: remaining.into(),
                    sep: rest_sep,
                    bracketed: false,
                    keywords: Some(kw),
                }),
            );
        } else if pos_iter.next().is_some() {
            return Err(Error::unpositioned(format!(
                "{name} was passed too many arguments."
            )));
        }
        // Reject keyword arguments that name no declared parameter. A `...`
        // rest parameter would absorb them into an arglist (whose keywords
        // are not yet modelled), so only validate when there is no rest.
        if params.rest.is_none() && !keyword.is_empty() {
            let leftover: Vec<&str> = keyword_order
                .iter()
                .filter(|(norm, _)| keyword.contains_key(norm))
                .map(|(_, orig)| orig.as_str())
                .collect();
            if let Some((last, init)) = leftover.split_last() {
                let msg = if init.is_empty() {
                    format!("No parameter named ${last}.")
                } else {
                    let head = init
                        .iter()
                        .map(|n| format!("${n}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("No parameters named {head} or ${last}.")
                };
                return Err(Error::unpositioned(msg));
            }
        }
        Ok(frame)
    }

    /// Call a user-defined `@function`, returning its `@return` value. `call`,
    /// when present, is the (name-start position, byte length) of the call
    /// expression, recorded as a diagnostic stack frame around the body.
    pub(super) fn call_function(
        &mut self,
        func: &Rc<UserCallable>,
        args: &[CallArg],
        call: Option<(Pos, usize)>,
    ) -> Result<Value, Error> {
        // Arguments evaluate in the CALLER's environment; the body (and the
        // parameter defaults) run against the callable's LEXICAL closure.
        let evaled = self.eval_call_args(args)?;
        let saved = call.map(|(pos, len)| self.enter_call(pos, len, &format!("{}()", func.def.name)));
        let saved_scopes = std::mem::replace(&mut self.scopes, func.env.clone());
        let saved_semi = std::mem::replace(&mut self.scope_semi_global, func.env_semi.clone());
        let saved_fns = std::mem::replace(&mut self.functions, func.env_fns.clone());
        let saved_mixins = std::mem::replace(&mut self.mixins, func.env_mixins.clone());
        self.push_scope(false);
        let result = self
            .bind_evaled_into_scope(&func.def.params, evaled, &func.def.name)
            .and_then(|()| {
                // A function body is not a mixin body: `meta.content-exists()`
                // called from a function (even one invoked by a mixin) errors.
                self.in_mixin.push(false);
                let r = self.run_fn_body(&func.def.body);
                self.in_mixin.pop();
                r
            });
        self.pop_scope();
        self.scopes = saved_scopes;
        self.scope_semi_global = saved_semi;
        self.functions = saved_fns;
        self.mixins = saved_mixins;
        if let Some(saved) = saved {
            self.leave_call(saved);
        }
        match result? {
            // A bare slash-division returned from a function collapses to
            // its number (dart-sass `withoutSlash`); slashes nested in a
            // returned list are preserved.
            Some(v) => Ok(v.without_slash()),
            None => Err(Error::unpositioned(format!(
                "Function {}() did not @return a value.",
                func.def.name
            ))),
        }
    }

    /// Run a function body, propagating the first `@return` (including from
    /// nested control flow). Functions emit no CSS, so a returned value
    /// short-circuits the whole call.
    pub(super) fn run_fn_body(&mut self, stmts: &[Stmt]) -> Result<Option<Value>, Error> {
        for stmt in stmts {
            match stmt {
                Stmt::VarDecl(v) => self.apply_var(v)?,
                Stmt::Comment(..) => {}
                Stmt::Return(e) => return Ok(Some(self.eval_expr(e)?)),
                Stmt::FunctionDef(c) => {
                    let captured = self.capture_callable(c);
                    self.define_function(&c.name, captured);
                }
                Stmt::If(branches) => {
                    for branch in branches {
                        let take = match &branch.cond {
                            None => true,
                            Some(c) => self.eval_expr(c)?.is_truthy(),
                        };
                        if take {
                            self.push_scope(true);
                            let result = self.run_fn_body(&branch.body);
                            self.pop_scope();
                            if let Some(v) = result? {
                                return Ok(Some(v));
                            }
                            break;
                        }
                    }
                }
                Stmt::For {
                    var,
                    from,
                    to,
                    inclusive,
                    body,
                } => {
                    let (start_i, end_i, unit) = self.for_bounds(from, to)?;
                    self.push_scope(true);
                    let mut result = Ok(None);
                    for i in for_indices(start_i, end_i, *inclusive) {
                        self.set_local(var, Value::Number(Number::with_unit(i as f64, unit.clone())));
                        result = self.run_fn_body(body);
                        if matches!(result, Ok(None)) {
                            continue;
                        }
                        break;
                    }
                    self.pop_scope();
                    if let Some(v) = result? {
                        return Ok(Some(v));
                    }
                }
                Stmt::Each { vars, list, body } => {
                    let items = self.eval_each_items(list)?;
                    self.push_scope(true);
                    let mut result = Ok(None);
                    for item in items {
                        self.bind_each(vars, item);
                        result = self.run_fn_body(body);
                        if matches!(result, Ok(None)) {
                            continue;
                        }
                        break;
                    }
                    self.pop_scope();
                    if let Some(v) = result? {
                        return Ok(Some(v));
                    }
                }
                Stmt::While { cond, body } => {
                    self.push_scope(true);
                    let mut result: Result<Option<Value>, Error> = Ok(None);
                    let mut guard = 0u32;
                    loop {
                        match self.eval_expr(cond) {
                            Ok(v) if v.is_truthy() => {}
                            Ok(_) => break,
                            Err(e) => {
                                result = Err(e);
                                break;
                            }
                        }
                        result = self.run_fn_body(body);
                        if !matches!(result, Ok(None)) {
                            break;
                        }
                        guard += 1;
                        if guard >= 100_000 {
                            result = Err(Error::unpositioned("@while exceeded 100000 iterations"));
                            break;
                        }
                    }
                    self.pop_scope();
                    if let Some(v) = result? {
                        return Ok(Some(v));
                    }
                }
                Stmt::Warn { value, pos } => self.emit_warn(value, *pos)?,
                Stmt::Debug { value, pos } => self.emit_debug(value, *pos)?,
                Stmt::Error { value, pos, length } => {
                    return Err(self.build_error(value, *pos, *length));
                }
                _ => {
                    return Err(Error::unpositioned(
                        "only variable assignments, control flow and @return are allowed in a function.",
                    ));
                }
            }
        }
        Ok(None)
    }

    /// Execute an `@include`: bind args into a call frame, make the content
    /// block available, and run the mixin body into the current sink.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn exec_include(
        &mut self,
        name: &str,
        args: &[CallArg],
        content: Option<Rc<Vec<Stmt>>>,
        content_params: Option<Rc<ParamList>>,
        module: Option<&str>,
        pos: Pos,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // NOTE: the diagnostic call frame for this `@include` is pushed by the
        // caller (the `Stmt::Include` arm) so it wraps every resolution path.
        // The built-in `@include meta.apply(...)` / `meta.load-css(...)` are
        // bound to the `sass:meta` namespace, so resolve them before the generic
        // module path.
        if let Some(ns) = module {
            if self.used_modules.get(ns).map(String::as_str) == Some("meta") {
                if name == "apply" {
                    return self.exec_apply(args, content, content_params, parents, sink);
                }
                if name == "load-css" {
                    return self.exec_load_css(args, content, pos, parents, sink);
                }
            }
        }
        // A namespaced `@include ns.mixin`: resolve a user module bound to the
        // namespace, then a built-in (which exposes no mixins in this build).
        if let Some(ns) = module {
            if let Some(target) = self.used_user_modules.get(ns).cloned() {
                if is_private_member(name) {
                    return Err(Error::unpositioned(
                        "Private members can't be accessed from outside their modules.",
                    ));
                }
                let mixin = target
                    .mixin(name)
                    .ok_or_else(|| Error::unpositioned("Undefined mixin."))?;
                // A forwarded mixin runs in its DEFINING module's environment.
                let exec = target.mixin_origin(name).unwrap_or(target);
                return self.run_module_mixin(&exec, &mixin, args, content, content_params, parents, sink);
            }
            if !self.used_modules.contains_key(ns) {
                return Err(Error::unpositioned(format!(
                    "There is no module with the namespace \"{ns}\"."
                )));
            }
            return Err(Error::unpositioned("Undefined mixin."));
        }
        // A bare `@include` may resolve a user module mixin exposed unprefixed
        // via `@use … as *`.
        if self.lookup_mixin(name).is_none() && !self.star_user_modules.is_empty() && !is_private_member(name)
        {
            let hits: Vec<(Rc<Module>, Rc<UserCallable>)> = self
                .star_user_modules
                .iter()
                .filter_map(|m| m.mixin(name).map(|mx| (Rc::clone(m), mx)))
                .collect();
            if hits.len() > 1 {
                return Err(Error::unpositioned(
                    "This mixin is available from multiple global modules.",
                ));
            }
            if let Some((m, mx)) = hits.into_iter().next() {
                return self.run_module_mixin(&m, &mx, args, content, content_params, parents, sink);
            }
        }
        let mixin = self
            .lookup_mixin(name)
            .ok_or_else(|| Error::unpositioned(format!("Undefined mixin {name}.")))?;
        // dart-sass: passing a content block to a mixin that never uses
        // `@content` is an error, even when the block is empty.
        if content.is_some() && !body_uses_content(&mixin.def.body) {
            return Err(Error::unpositioned("Mixin doesn't accept a content block."));
        }
        // Arguments evaluate in the caller's environment; the body runs in
        // the mixin's lexical closure. The content block captures the CALL
        // SITE so `@content` sees the includer's variables.
        let evaled = self.eval_call_args(args)?;
        let content_block = content.map(|stmts| {
            let snapshot = self.snapshot_env();
            ContentBlock {
                stmts,
                params: content_params.clone(),
                caller_env: Some(Box::new(snapshot)),
            }
        });
        let saved_scopes = std::mem::replace(&mut self.scopes, mixin.env.clone());
        let saved_semi = std::mem::replace(&mut self.scope_semi_global, mixin.env_semi.clone());
        let saved_fns = std::mem::replace(&mut self.functions, mixin.env_fns.clone());
        let saved_mixins = std::mem::replace(&mut self.mixins, mixin.env_mixins.clone());
        self.push_scope(false);
        let result = self
            .bind_evaled_into_scope(&mixin.def.params, evaled, &mixin.def.name)
            .and_then(|()| {
                self.content_stack.push(content_block);
                self.in_mixin.push(true);
                let r = self.exec(&mixin.def.body, parents, sink);
                self.in_mixin.pop();
                self.content_stack.pop();
                r
            });
        self.pop_scope();
        self.scopes = saved_scopes;
        self.scope_semi_global = saved_semi;
        self.functions = saved_fns;
        self.mixins = saved_mixins;
        result
    }

    /// Execute an `@include ns.mixin` where `ns` is a user module: run the mixin
    /// body in the module's own environment, while its `@content` block (if any)
    /// runs back in the call site's environment.
    #[allow(clippy::too_many_arguments)]
    fn run_module_mixin(
        &mut self,
        module: &Rc<Module>,
        mixin: &Rc<UserCallable>,
        args: &[CallArg],
        content: Option<Rc<Vec<Stmt>>>,
        content_params: Option<Rc<ParamList>>,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        if content.is_some() && !body_uses_content(&mixin.def.body) {
            return Err(Error::unpositioned("Mixin doesn't accept a content block."));
        }
        // Evaluate the arguments at the call site (so they resolve in the
        // caller's scope), then enter the module's environment and the
        // mixin's lexical closure for the body. Snapshot the call-site env
        // so a `@content` block runs there, not in the module.
        let evaled = self.eval_call_args(args)?;
        let content_block = content.map(|stmts| {
            let snapshot = self.snapshot_env();
            ContentBlock {
                stmts,
                params: content_params.clone(),
                caller_env: Some(Box::new(snapshot)),
            }
        });
        let saved = self.enter_module(module);
        let saved_file = self.enter_module_file(module);
        let saved_scopes = std::mem::replace(&mut self.scopes, mixin.env.clone());
        let saved_semi = std::mem::replace(&mut self.scope_semi_global, mixin.env_semi.clone());
        let saved_fns = std::mem::replace(&mut self.functions, mixin.env_fns.clone());
        let saved_mixins = std::mem::replace(&mut self.mixins, mixin.env_mixins.clone());
        self.push_scope(false);
        let result = self
            .bind_evaled_into_scope(&mixin.def.params, evaled, &mixin.def.name)
            .and_then(|()| {
                self.content_stack.push(content_block);
                let r = self.exec(&mixin.def.body, parents, sink);
                self.content_stack.pop();
                r
            });
        self.pop_scope();
        self.scopes = saved_scopes;
        self.scope_semi_global = saved_semi;
        self.functions = saved_fns;
        self.mixins = saved_mixins;
        self.leave_module_file(saved_file);
        self.leave_module(saved);
        result
    }

    /// `@include meta.apply($mixin, $args...)`: invoke a first-class mixin
    /// reference. The first argument is the mixin reference; the rest are the
    /// arguments passed on to that mixin (which may also accept a `@content`
    /// block).
    fn exec_apply(
        &mut self,
        args: &[CallArg],
        content: Option<Rc<Vec<Stmt>>>,
        content_params: Option<Rc<ParamList>>,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // Evaluate apply's own arguments (expanding any `...` splat). The first
        // positional (or named `$mixin`) is the mixin reference; the remainder
        // are forwarded to the mixin.
        let (mut pos_args, mut named, _) = self.eval_call_args(args)?;
        for v in &mut pos_args {
            *v = std::mem::replace(v, Value::Null).without_slash();
        }
        for (_, v) in &mut named {
            *v = std::mem::replace(v, Value::Null).without_slash();
        }
        let (mixin_val, rest_pos): (Value, Vec<Value>) = if !pos_args.is_empty() {
            let mut iter = pos_args.into_iter();
            let first = iter.next().unwrap_or(Value::Null);
            (first, iter.collect())
        } else if let Some(idx) = named.iter().position(|(n, _)| n == "mixin") {
            (named.remove(idx).1, Vec::new())
        } else {
            return Err(Error::unpositioned("Missing argument $mixin."));
        };
        let rest_named: Vec<(String, Value)> = named.into_iter().filter(|(n, _)| n != "mixin").collect();
        let mixin = match mixin_val {
            Value::Mixin(m) => m,
            other => {
                return Err(Error::unpositioned(format!(
                    "$mixin: {} is not a mixin reference.",
                    other.to_css(false)
                )))
            }
        };
        self.invoke_mixin_ref(
            &mixin,
            rest_pos,
            rest_named,
            content,
            content_params,
            parents,
            sink,
        )
    }

    /// Invoke a resolved mixin reference with already-evaluated arguments and an
    /// optional `@content` block, emitting into `sink`.
    #[allow(clippy::too_many_arguments)]
    fn invoke_mixin_ref(
        &mut self,
        mixin: &SassMixin,
        pos_args: Vec<Value>,
        named: Vec<(String, Value)>,
        content: Option<Rc<Vec<Stmt>>>,
        content_params: Option<Rc<ParamList>>,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // A captured user `@mixin`: recover the type-erased `Callable`.
        let callable = match &mixin.user {
            Some(any) => match Rc::clone(any).downcast::<UserCallable>() {
                Ok(c) => c,
                Err(_) => return Err(Error::unpositioned("Undefined mixin.")),
            },
            // A built-in mixin reference (`meta.load-css`/`meta.apply`). Only the
            // content-block validation is observable in the supported cases.
            None => {
                if content.is_some() {
                    return Err(Error::unpositioned("Mixin doesn't accept a content block."));
                }
                return Err(Error::unpositioned("Undefined mixin."));
            }
        };
        if content.is_some() && !body_uses_content(&callable.def.body) {
            return Err(Error::unpositioned("Mixin doesn't accept a content block."));
        }
        let content_block = content.map(|stmts| {
            let snapshot = self.snapshot_env();
            ContentBlock {
                stmts,
                params: content_params.clone(),
                caller_env: Some(Box::new(snapshot)),
            }
        });
        // A mixin captured from another module runs in that module's
        // environment; either way the body runs in its lexical closure and
        // the `@content` block runs back at the call site.
        let module = mixin
            .module
            .as_ref()
            .and_then(|m| Rc::clone(m).downcast::<Module>().ok());
        let saved = module.as_ref().map(|m| self.enter_module(m));
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
                self.content_stack.push(content_block);
                self.in_mixin.push(true);
                let r = self.exec(&callable.def.body, parents, sink);
                self.in_mixin.pop();
                self.content_stack.pop();
                r
            });
        self.pop_scope();
        self.scopes = saved_scopes;
        self.scope_semi_global = saved_semi;
        self.functions = saved_fns;
        self.mixins = saved_mixins;
        if let Some(saved) = saved {
            self.leave_module(saved);
        }
        result
    }

    /// Run the innermost active `@content` block. For a cross-module include the
    /// block carries a snapshot of the call-site environment, which is installed
    /// for the duration so the content resolves there rather than in the mixin's
    /// module.
    pub(super) fn exec_content(
        &mut self,
        args: &[CallArg],
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let (stmts, params, caller_env) = match self.content_stack.last() {
            Some(Some(block)) => (
                Rc::clone(&block.stmts),
                block.params.clone(),
                block.caller_env.as_ref().map(|e| (**e).clone()),
            ),
            _ => return Ok(()),
        };
        // `@content(args)` evaluates its arguments at the call site (the mixin
        // body), then binds them to the content block's `using (params)`, which
        // become visible inside the block.
        let frame = match &params {
            Some(p) => Some(self.bind_args(p, args, "@content")?),
            None => {
                // A content block with no `using (params)` accepts no
                // arguments; passing any is an error (dart-sass).
                if !args.is_empty() {
                    let n = args.len();
                    let verb = if n == 1 { "was" } else { "were" };
                    return Err(Error::unpositioned(format!(
                        "Only 0 arguments allowed, but {n} {verb} passed."
                    )));
                }
                None
            }
        };
        let restore = caller_env.map(|env| self.install_env(env));
        // A content block is a user-defined callable in dart: its body always
        // runs in a fresh child scope, so a `$var:` first declared inside it
        // stays local to the block (and a `using` frame binds there).
        match frame {
            Some(frame) => self.push_scope_frame(frame),
            None => self.push_scope(false),
        }
        // The block runs in its DEFINITION environment's content context: a
        // `@content` inside it forwards to the block one level up, not to
        // itself (a recursive mixin chaining `@content` must terminate).
        let running = self.content_stack.pop();
        let result = self.exec(&stmts, parents, sink);
        if let Some(top) = running {
            self.content_stack.push(top);
        }
        self.pop_scope();
        if let Some(restore) = restore {
            self.leave_module(restore);
        }
        result
    }

    /// The lazy `if($condition, $if-true, $if-false)` function: evaluates
    /// the condition, then only the selected branch.
    pub(super) fn eval_if_function(&mut self, args: &[CallArg], pos: Pos) -> Result<Value, Error> {
        // An argument is lazy (an unevaluated branch expression) unless it
        // came from a `...` splat: dart's macro-argument handling evaluates
        // the splat eagerly and reconstitutes its elements (and an argument
        // list's keywords) as already-evaluated arguments.
        enum IfArg<'a> {
            Lazy(&'a Expr),
            Eager(Value),
        }
        fn slot_index(name: &str) -> Option<usize> {
            match name {
                "condition" => Some(0),
                "if-true" => Some(1),
                "if-false" => Some(2),
                _ => None,
            }
        }
        let mut by_pos: Vec<IfArg<'_>> = Vec::new();
        // $condition / $if-true / $if-false by name.
        let mut named: [Option<IfArg<'_>>; 3] = [None, None, None];
        for a in args {
            if a.splat {
                match self.eval_expr(&a.value)? {
                    Value::List(l) => {
                        if let Some(kw) = &l.keywords {
                            for (k, v) in kw {
                                if let Value::Str(s) = k {
                                    match slot_index(&s.text) {
                                        Some(i) => named[i] = Some(IfArg::Eager(v.clone())),
                                        None => {
                                            return Err(Error::at(
                                                format!("if() has no argument named ${}.", s.text),
                                                pos,
                                            ))
                                        }
                                    }
                                }
                            }
                        }
                        for item in l.items.iter().cloned() {
                            by_pos.push(IfArg::Eager(item));
                        }
                    }
                    Value::Map(m) => {
                        for (k, v) in m.entries.as_ref().clone() {
                            let name = match k {
                                Value::Str(s) => s.text,
                                other => {
                                    return Err(Error::at(
                                        format!(
                                            "Variable keyword argument map must have string keys.\n{} is not a string.",
                                            other.to_css(false)
                                        ),
                                        pos,
                                    ))
                                }
                            };
                            match slot_index(&name) {
                                Some(i) => named[i] = Some(IfArg::Eager(v)),
                                None => {
                                    return Err(Error::at(
                                        format!("if() has no argument named ${name}."),
                                        pos,
                                    ))
                                }
                            }
                        }
                    }
                    other => by_pos.push(IfArg::Eager(other)),
                }
                continue;
            }
            match a.name.as_deref() {
                Some(name) => match slot_index(name) {
                    Some(i) => named[i] = Some(IfArg::Lazy(&a.value)),
                    None => {
                        return Err(Error::at(format!("if() has no argument named ${name}."), pos));
                    }
                },
                None => by_pos.push(IfArg::Lazy(&a.value)),
            }
        }
        let [cond, t_val, f_val] = named;
        let mut pos_iter = by_pos.into_iter();
        let cond = cond.or_else(|| pos_iter.next());
        let t_val = t_val.or_else(|| pos_iter.next());
        let f_val = f_val.or_else(|| pos_iter.next());
        match (cond, t_val, f_val) {
            (Some(c), Some(t), Some(f)) => {
                let truthy = match c {
                    IfArg::Lazy(e) => self.eval_expr(e)?.is_truthy(),
                    IfArg::Eager(v) => v.is_truthy(),
                };
                // if() is a function boundary: a bare slash-division branch
                // collapses to its number (dart-sass `withoutSlash`).
                let branch = if truthy { t } else { f };
                match branch {
                    IfArg::Lazy(e) => Ok(self.eval_expr(e)?.without_slash()),
                    IfArg::Eager(v) => Ok(v.without_slash()),
                }
            }
            _ => Err(Error::at(
                "if() requires arguments $condition, $if-true, $if-false.",
                pos,
            )),
        }
    }

    /// Evaluate a modern CSS `if()`: a `;`-separated list of clauses, each
    /// `<condition>: <value>` (or `else: <value>`). Conditions mix evaluated
    /// `sass(<expr>)` with non-evaluable `css(...)` / arbitrary substitution
    /// pieces. If every reachable condition resolves statically, the matching
    /// value is returned; otherwise the whole `if()` is re-serialized
    /// verbatim (with statically-true/false conditions folded away) as an
    /// unquoted string.
    pub(super) fn eval_modern_if(&mut self, clauses: &[IfClause]) -> Result<Value, Error> {
        let mut verbatim: Option<Vec<String>> = None;
        for clause in clauses {
            // The `else` clause has no condition: it always matches.
            let result = match &clause.condition {
                None => CondEval::Bool(true),
                Some(cond) => self.eval_if_cond(cond)?,
            };
            match (&mut verbatim, result) {
                // Not yet verbatim: a static-true (or `else`) clause wins.
                (None, CondEval::Bool(true)) => {
                    return Ok(self.eval_expr(&clause.value)?.without_slash());
                }
                // Not yet verbatim: a static-false clause is skipped.
                (None, CondEval::Bool(false)) => {}
                // First non-evaluable condition: enter verbatim mode.
                (None, CondEval::Css(rc)) => {
                    let value = self.eval_if_value(&clause.value)?;
                    verbatim = Some(vec![format!("{}: {}", rc.to_css(), value)]);
                }
                // Already verbatim: fold each remaining clause.
                (Some(out), CondEval::Bool(true)) => {
                    let value = self.eval_if_value(&clause.value)?;
                    out.push(format!("else: {value}"));
                }
                (Some(_), CondEval::Bool(false)) => {}
                (Some(out), CondEval::Css(rc)) => {
                    let value = self.eval_if_value(&clause.value)?;
                    out.push(format!("{}: {}", rc.to_css(), value));
                }
            }
        }
        match verbatim {
            Some(parts) => Ok(Value::Str(SassStr {
                text: format!("if({})", parts.join("; ")).into(),
                quoted: false,
            })),
            // No clause matched and no `else`: the modern `if()` is null.
            None => Ok(Value::Null),
        }
    }

    /// Evaluate an `if()` clause value. dart-sass serializes the value in a
    /// parenthesized-expression context, so lists are wrapped in parens and
    /// a bare slash-division collapses to its number.
    fn eval_if_value(&mut self, expr: &Expr) -> Result<String, Error> {
        let v = self.eval_expr(expr)?.without_slash();
        Ok(serialize_if_value(&v))
    }

    /// Evaluate a modern `if()` condition into a tri-state result: a static
    /// boolean (from `sass(...)` atoms) or a residual non-evaluable CSS
    /// condition that must be re-serialized verbatim.
    fn eval_if_cond(&mut self, cond: &IfCond) -> Result<CondEval, Error> {
        match cond {
            IfCond::Sass(expr) => Ok(CondEval::Bool(self.eval_expr(expr)?.is_truthy())),
            IfCond::Raw { pieces, .. } => {
                let text = self.eval_template(pieces)?;
                // dart re-serializes the raw token run with collapsed
                // whitespace and no space inside empty parens
                // (`css(\n)` is `css()`).
                let mut collapsed = String::with_capacity(text.len());
                let mut prev_ws = false;
                for c in text.chars() {
                    if c.is_whitespace() {
                        prev_ws = true;
                        continue;
                    }
                    if prev_ws && !collapsed.is_empty() && c != ')' {
                        collapsed.push(' ');
                    }
                    prev_ws = false;
                    collapsed.push(c);
                }
                let collapsed = collapsed.replace("( ", "(");
                Ok(CondEval::Css(RCond::Css(collapsed)))
            }
            IfCond::Not(inner) => match self.eval_if_cond(inner)? {
                CondEval::Bool(b) => Ok(CondEval::Bool(!b)),
                CondEval::Css(rc) => Ok(CondEval::Css(RCond::Not(Box::new(rc)))),
            },
            IfCond::Paren(inner) => match self.eval_if_cond(inner)? {
                CondEval::Bool(b) => Ok(CondEval::Bool(b)),
                CondEval::Css(rc) => Ok(CondEval::Css(RCond::Paren(Box::new(rc)))),
            },
            IfCond::And(items) => {
                let mut residuals: Vec<RCond> = Vec::new();
                for item in items {
                    match self.eval_if_cond(item)? {
                        // A statically-false operand makes the whole `and`
                        // false and short-circuits the rest.
                        CondEval::Bool(false) => return Ok(CondEval::Bool(false)),
                        // A statically-true operand drops out of the `and`.
                        CondEval::Bool(true) => {}
                        CondEval::Css(rc) => residuals.push(rc),
                    }
                }
                Ok(combine_residuals(residuals, true))
            }
            IfCond::Or(items) => {
                let mut residuals: Vec<RCond> = Vec::new();
                for item in items {
                    match self.eval_if_cond(item)? {
                        // A statically-true operand makes the whole `or`
                        // true and short-circuits the rest.
                        CondEval::Bool(true) => return Ok(CondEval::Bool(true)),
                        // A statically-false operand drops out of the `or`.
                        CondEval::Bool(false) => {}
                        CondEval::Css(rc) => residuals.push(rc),
                    }
                }
                Ok(combine_residuals(residuals, false))
            }
        }
    }
}
