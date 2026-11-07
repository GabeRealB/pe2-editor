//! `#[steps]`: an operation cut into steps where a `let` is marked - `#[request]`, a read of other nodes made before the
//! next step; `#[each]`, a map whose items each run as a micro op; `#[call]`, a helper in steps - wherever a statement
//! stands in its body's blocks, `if`s, `match`es and loops. What lives across a cut is carried to the next step
//! (`pe2_ext::stepped`).
//!
//! The body becomes a step from its start and one after each cut, each running to the next cut or the end. The step after
//! a cut is what follows it, the constructs around it built again: the rest of its block, then what follows the block in
//! the one holding it, and so on out. Around a loop it is a loop again, its first turn the rest of the turn the cut was
//! in, its later turns the body as written, so that `break` and `continue` mean what they did.

use proc_macro2::{Span, TokenStream, TokenTree};
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::visit_mut::{self, VisitMut};
use syn::{
    Arm, Expr, ExprClosure, FnArg, GenericArgument, Ident, ItemFn, Label, Pat, PathArguments, ReturnType, Stmt, Type,
};

/// A cut: what its `let` binds, and what it requests or spawns.
struct Cut {
    ident: Ident,
    mutable: bool,
    ty: Type,
    kind: Kind,
}

enum Kind {
    /// A read of other nodes: the `(node, address)` pairs it names.
    Request(Box<Expr>),
    /// A map: its closure run for each item; `made` is what it makes (the `Vec<_>`'s item).
    Each { items: Box<Expr>, item: Box<Pat>, item_ty: Box<Type>, body: Box<Expr>, made: Box<Type> },
    /// A map naming a helper in steps, run for each item.
    EachSteps { items: Box<Expr>, helper: Box<Expr> },
    /// A helper in steps, run once with these arguments.
    Call { helper: Box<Expr>, args: Vec<Expr> },
}

/// A branch of an `if`: its condition (none for the `else`) and its body.
type Branch = (Option<Box<Expr>>, Vec<Node>);

/// The body as steps see it: statements, cuts by their number, and the constructs holding cuts.
enum Node {
    Stmt(Box<Stmt>),
    Cut(usize),
    Block {
        label: Option<Label>,
        body: Vec<Node>,
    },
    /// `if`, `else if`s and an `else`: each its condition (none for the `else`) and body.
    If(Vec<Branch>),
    /// A `match`: what it matches, and each arm's head (its pattern and guard) and body.
    Match {
        on: Box<Expr>,
        arms: Vec<(TokenStream, Vec<Node>)>,
    },
    Loop {
        label: Option<Label>,
        body: Vec<Node>,
    },
    While {
        label: Option<Label>,
        cond: Box<Expr>,
        body: Vec<Node>,
    },
    /// A `for` over a variable holding a `Vec`, gone through by an index (`index`), carried like any variable.
    For {
        label: Option<Label>,
        pat: Box<Pat>,
        items: Ident,
        index: Ident,
        body: Vec<Node>,
    },
}

/// A variable bound in the body: its type where it is written, and how deep in the body's constructs.
#[derive(Clone)]
struct Bound {
    ident: Ident,
    ty: Option<Type>,
    mutable: bool,
    depth: usize,
}

pub fn expand(function: &ItemFn) -> TokenStream {
    match steps(function) {
        Ok(tokens) => tokens,
        Err(e) => e.to_compile_error(),
    }
}

fn error(span: Span, what: &str) -> syn::Error {
    syn::Error::new(span, what)
}

const PLACE: &str = "a cut stands where a statement does, in blocks, `if`s, `match` arms and loops - not in an expression, a \
                     closure or another function";

fn steps(function: &ItemFn) -> syn::Result<TokenStream> {
    let signature = &function.sig;
    if !signature.generics.params.is_empty() {
        return Err(error(signature.generics.span(), "an operation in steps takes no generics"));
    }
    let mut args = signature.inputs.iter();
    let call = match args.next() {
        Some(FnArg::Typed(arg)) => match &*arg.pat {
            Pat::Ident(ident) => (ident.ident.clone(), (*arg.ty).clone()),
            other => return Err(error(other.span(), "its call is named by one variable")),
        },
        _ => return Err(error(signature.inputs.span(), "an operation in steps takes its call first")),
    };
    let params = args.map(parameter).collect::<syn::Result<Vec<_>>>()?;
    let ReturnType::Type(_, returns) = &signature.output else {
        return Err(error(signature.span(), "an operation in steps returns `Result<T>`"));
    };
    let made = inner_of(returns, "a `Result<T>`")?;
    let mut block = (*function.block).clone();
    Returns.visit_block_mut(&mut block);
    let mut parser = Parser { cuts: Vec::new(), fors: 0 };
    let program = parser.nodes(&block.stmts)?;
    if parser.cuts.is_empty() {
        return Err(error(signature.span(), "an operation in steps has a cut: `#[request]`, `#[each]` or `#[call]`"));
    }
    let writer = Writer { call: call.0, call_ty: call.1, handed: Ident::new("__handed", Span::mixed_site()) };
    let paths = paths(&program);
    let carried = carried(&writer, &program, &parser.cuts, &paths, &params)?;
    let mut fns = Vec::new();
    // The first step: the parameters after its call are its arguments, as a helper is called.
    let restore_args = (!params.is_empty()).then(|| {
        let (idents, types) = (params.iter().map(binding), params.iter().filter_map(|p| p.ty.as_ref()));
        let handed = &writer.handed;
        quote!(let (#(#idents,)*): (#(#types,)*) = #handed.item()?;)
    });
    let (start, _) = writer.code(&program, 0, true, &parser.cuts, Some(&carried));
    fns.push(writer.entry(0, &made, &quote!(#restore_args), &start));
    for (cut, path) in paths.iter().enumerate() {
        let (code, _) = writer.after(&program, path, &parser.cuts, Some(&carried));
        fns.push(writer.entry(cut + 1, &made, &writer.restore(cut, &parser.cuts, &carried[cut]), &code));
    }
    let mut chain: Vec<TokenStream> = (0..=parser.cuts.len())
        .map(|at| {
            let entry = format_ident!("__entry{}", at);
            quote!(.entry(#entry))
        })
        .collect();
    for (at, cut) in parser.cuts.iter().enumerate() {
        chain.push(match &cut.kind {
            Kind::Request(_) => quote!(.cut()),
            Kind::Each { item, item_ty, body, made, .. } => {
                let each = format_ident!("__each{}", at);
                let (call, call_ty) = (&writer.call, &writer.call_ty);
                fns.push(quote! {
                    #[allow(unused_mut, unused_variables, clippy::all, clippy::pedantic)]
                    fn #each(#call: #call_ty, __item: &[u8]) -> ::pe2_ext::Result<::std::vec::Vec<u8>> {
                        let #item: #item_ty = ::pe2_ext::stepped::from_bytes(__item)?;
                        let __made: #made = (|| -> ::pe2_ext::Result<#made> { ::core::result::Result::Ok(#body) })()?;
                        ::pe2_ext::stepped::to_bytes(&__made)
                    }
                });
                quote!(.cut_each(#each))
            }
            Kind::EachSteps { helper, .. } | Kind::Call { helper, .. } => quote!(.cut_steps(#helper)),
        });
    }
    let (vis, name, attrs) = (&function.vis, &signature.ident, &function.attrs);
    Ok(quote! {
        #(#attrs)*
        #vis fn #name() -> ::pe2_ext::stepped::Steps<#made> {
            #(#fns)*
            ::pe2_ext::stepped::Steps::new() #(#chain)*
        }
    })
}

/// A parameter after the call: an argument, its name and type.
fn parameter(arg: &FnArg) -> syn::Result<Bound> {
    match arg {
        FnArg::Typed(arg) => match &*arg.pat {
            Pat::Ident(ident) => Ok(Bound {
                ident: ident.ident.clone(),
                ty: Some((*arg.ty).clone()),
                mutable: ident.mutability.is_some(),
                depth: 0,
            }),
            other => Err(error(other.span(), "a parameter is named by one variable")),
        },
        FnArg::Receiver(other) => Err(error(other.span(), "an operation in steps is a function")),
    }
}

fn binding(bound: &Bound) -> TokenStream {
    let (ident, mutable) = (&bound.ident, bound.mutable.then(|| quote!(mut)));
    quote!(#mutable #ident)
}

/// `return` in the body finishes the operation, as every step's end does.
struct Returns;

impl VisitMut for Returns {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        match expr {
            Expr::Closure(_) | Expr::Async(_) => {}
            Expr::Return(returned) => {
                if let Some(value) = &mut returned.expr {
                    self.visit_expr_mut(value);
                    let value = &**value;
                    *expr = syn::parse_quote!(return (#value).map(::pe2_ext::stepped::Flow::Done));
                }
            }
            _ => visit_mut::visit_expr_mut(self, expr),
        }
    }

    fn visit_item_mut(&mut self, _: &mut syn::Item) {}
}

/// What reads the body into nodes, numbering its cuts and its `for`s' indices.
struct Parser {
    cuts: Vec<Cut>,
    fors: usize,
}

impl Parser {
    fn nodes(&mut self, stmts: &[Stmt]) -> syn::Result<Vec<Node>> {
        stmts.iter().map(|stmt| self.node(stmt)).collect()
    }

    fn node(&mut self, stmt: &Stmt) -> syn::Result<Node> {
        if let Some(cut) = cut_of(stmt)? {
            self.cuts.push(cut);
            return Ok(Node::Cut(self.cuts.len() - 1));
        }
        let Stmt::Expr(expr, _) = stmt else {
            no_cut(stmt)?;
            return Ok(Node::Stmt(Box::new(stmt.clone())));
        };
        if !holds_cut(expr) {
            return Ok(Node::Stmt(Box::new(stmt.clone())));
        }
        match expr {
            Expr::Block(block) => Ok(Node::Block { label: block.label.clone(), body: self.nodes(&block.block.stmts)? }),
            Expr::If(_) => Ok(Node::If(self.branches(expr)?)),
            Expr::Match(matched) => {
                let mut arms = Vec::new();
                for arm in &matched.arms {
                    let Arm { pat, guard, body, .. } = arm;
                    let guard = guard.as_ref().map(|(_, guard)| quote!(if #guard));
                    let body = match &**body {
                        Expr::Block(block) => self.nodes(&block.block.stmts)?,
                        other if holds_cut(other) => return Err(error(other.span(), PLACE)),
                        other => vec![Node::Stmt(Box::new(Stmt::Expr(other.clone(), None)))],
                    };
                    arms.push((quote!(#pat #guard), body));
                }
                Ok(Node::Match { on: matched.expr.clone(), arms })
            }
            Expr::Loop(looped) => Ok(Node::Loop { label: looped.label.clone(), body: self.nodes(&looped.body.stmts)? }),
            Expr::While(looped) => Ok(Node::While {
                label: looped.label.clone(),
                cond: looped.cond.clone(),
                body: self.nodes(&looped.body.stmts)?,
            }),
            Expr::ForLoop(looped) => {
                let Expr::Path(items) = &*looped.expr else {
                    return Err(error(
                        looped.expr.span(),
                        "a `for` holding a cut goes over a variable holding a `Vec`",
                    ));
                };
                let items = items
                    .path
                    .get_ident()
                    .cloned()
                    .ok_or_else(|| error(looped.expr.span(), "a `for` holding a cut goes over a variable"))?;
                let index = format_ident!("__index{}", self.fors);
                self.fors += 1;
                Ok(Node::For {
                    label: looped.label.clone(),
                    pat: looped.pat.clone(),
                    items,
                    index,
                    body: self.nodes(&looped.body.stmts)?,
                })
            }
            other => Err(error(other.span(), PLACE)),
        }
    }

    /// An `if` and its `else if`s and `else`, each its condition and body.
    fn branches(&mut self, expr: &Expr) -> syn::Result<Vec<Branch>> {
        let Expr::If(branch) = expr else { return Err(error(expr.span(), PLACE)) };
        let mut branches = vec![(Some(branch.cond.clone()), self.nodes(&branch.then_branch.stmts)?)];
        match branch.else_branch.as_ref().map(|(_, other)| &**other) {
            None => {}
            Some(next @ Expr::If(_)) => branches.extend(self.branches(next)?),
            Some(Expr::Block(block)) => branches.push((None, self.nodes(&block.block.stmts)?)),
            Some(other) => return Err(error(other.span(), PLACE)),
        }
        Ok(branches)
    }
}

/// Whether an expression holds a cut anywhere.
fn holds_cut(expr: &Expr) -> bool {
    struct Find(bool);
    impl<'ast> Visit<'ast> for Find {
        fn visit_local(&mut self, local: &'ast syn::Local) {
            self.0 |= marker_of(&local.attrs).is_some();
            visit::visit_local(self, local);
        }
    }
    let mut find = Find(false);
    find.visit_expr(expr);
    find.0
}

/// That a statement which is no construct holding cuts holds none: in an expression, a closure or a function.
fn no_cut(stmt: &Stmt) -> syn::Result<()> {
    struct Find(Option<Span>);
    impl<'ast> Visit<'ast> for Find {
        fn visit_local(&mut self, local: &'ast syn::Local) {
            if marker_of(&local.attrs).is_some() && self.0.is_none() {
                self.0 = Some(local.span());
            }
            visit::visit_local(self, local);
        }
    }
    let mut find = Find(None);
    match stmt {
        Stmt::Local(local) => {
            if let Some(init) = &local.init {
                find.visit_expr(&init.expr);
            }
        }
        other => find.visit_stmt(other),
    }
    find.0.map_or(Ok(()), |span| Err(error(span, PLACE)))
}

/// The marker on a `let`, where it has one.
fn marker_of(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        let ident = attr.path().get_ident()?.to_string();
        matches!(ident.as_str(), "request" | "each" | "call").then_some(ident)
    })
}

/// A cut, where the statement is a marked `let`.
fn cut_of(stmt: &Stmt) -> syn::Result<Option<Cut>> {
    let Stmt::Local(local) = stmt else { return Ok(None) };
    let Some(which) = marker_of(&local.attrs) else { return Ok(None) };
    let named = "a cut is a `let` naming one variable with its type (`let x: T = ...`)";
    let Pat::Type(typed) = &local.pat else { return Err(error(local.pat.span(), named)) };
    let Pat::Ident(ident) = &*typed.pat else { return Err(error(typed.pat.span(), named)) };
    let ty = (*typed.ty).clone();
    let Some(init) = &local.init else { return Err(error(local.span(), "a cut is a `let` with its value")) };
    let kind = match which.as_str() {
        "request" => request_of(&init.expr)?,
        "each" => each_of(&init.expr, &ty)?,
        _ => call_of(&init.expr)?,
    };
    Ok(Some(Cut { ident: ident.ident.clone(), mutable: ident.mutability.is_some(), ty, kind }))
}

/// A `#[request]`: `call.tables_of(pairs)?` or `call.outputs_of(pairs)?`, or one, `call.table_of(node, address)?` or
/// `call.output_of(node, address)?` - without the `?`, the read's own result, for what may find nothing.
fn request_of(expr: &Expr) -> syn::Result<Kind> {
    let shape = "a `#[request]` reads with `call.tables_of(pairs)?`, `outputs_of(pairs)?`, `outputs_tried(pairs)`, \
                 `table_of(node, address)` or `output_of(node, address)`, with its `?` or without";
    let read = match expr {
        Expr::Try(tried) => &*tried.expr,
        other => other,
    };
    let Expr::MethodCall(read) = read else { return Err(error(expr.span(), shape)) };
    let args: Vec<&Expr> = read.args.iter().collect();
    let pairs = match (read.method.to_string().as_str(), args.as_slice()) {
        ("tables_of" | "outputs_of" | "outputs_tried", [pairs]) => (*pairs).clone(),
        ("table_of" | "output_of", [node, address]) => syn::parse_quote!([(#node, #address)]),
        _ => return Err(error(read.span(), shape)),
    };
    Ok(Kind::Request(Box::new(pairs)))
}

/// An `#[each]`: `items.map(|item: I| body).collect()`, or `items.map(helper).collect()` naming a helper in steps.
fn each_of(expr: &Expr, ty: &Type) -> syn::Result<Kind> {
    let shape = "an `#[each]` is a map: `items.map(|item: T| body).collect()`, or `items.map(helper).collect()` naming a \
                 `#[steps]` function taking its item";
    let mut expr = expr;
    if let Expr::MethodCall(collect) = expr
        && collect.method == "collect"
        && collect.args.is_empty()
    {
        expr = &collect.receiver;
    }
    let Expr::MethodCall(map) = expr else { return Err(error(expr.span(), shape)) };
    let (true, Some(run), 1) = (map.method == "map", map.args.first(), map.args.len()) else {
        return Err(error(map.span(), shape));
    };
    let items = map.receiver.clone();
    match run {
        Expr::Closure(closure) => {
            let ExprClosure { inputs, body, .. } = closure;
            let mut inputs = inputs.iter();
            let (Some(Pat::Type(item)), None) = (inputs.next(), inputs.next()) else {
                return Err(error(closure.span(), "an `#[each]` closure takes one item, with its type (`|item: T|`)"));
            };
            let made = Box::new(inner_of(ty, "a `Vec<T>`: what each item makes")?);
            Ok(Kind::Each { items, item: item.pat.clone(), item_ty: item.ty.clone(), body: body.clone(), made })
        }
        helper @ Expr::Path(_) => Ok(Kind::EachSteps { items, helper: Box::new(helper.clone()) }),
        other => Err(error(other.span(), shape)),
    }
}

/// A `#[call]`: `helper(call, args...)?`, a helper in steps given its arguments after the call.
fn call_of(expr: &Expr) -> syn::Result<Kind> {
    let shape = "a `#[call]` calls a `#[steps]` helper: `helper(call, args...)?`";
    let expr = match expr {
        Expr::Try(tried) => &*tried.expr,
        other => other,
    };
    let Expr::Call(called) = expr else { return Err(error(expr.span(), shape)) };
    let Expr::Path(_) = &*called.func else { return Err(error(called.func.span(), shape)) };
    let args: Vec<Expr> = called.args.iter().skip(1).cloned().collect();
    Ok(Kind::Call { helper: called.func.clone(), args })
}

/// The `T` of a `Result<T>` or `Vec<T>`: the last segment's first type argument.
fn inner_of(ty: &Type, what: &str) -> syn::Result<Type> {
    if let Type::Path(path) = ty
        && let Some(last) = path.path.segments.last()
        && let PathArguments::AngleBracketed(args) = &last.arguments
        && let Some(GenericArgument::Type(inner)) = args.args.first()
    {
        return Ok(inner.clone());
    }
    Err(error(ty.span(), &format!("expected {what}")))
}

/// Where a cut stands: the places down to it, each a node's place in its list and which of its bodies holds the next.
type Path = Vec<(usize, usize)>;

/// Where each cut stands, by its number.
fn paths(program: &[Node]) -> Vec<Path> {
    fn walk(list: &[Node], path: &mut Path, out: &mut Vec<(usize, Path)>) {
        for (at, node) in list.iter().enumerate() {
            match node {
                Node::Cut(cut) => {
                    let mut found = path.clone();
                    found.push((at, 0));
                    out.push((*cut, found));
                }
                Node::Stmt(_) => {}
                other => {
                    for (which, body) in bodies(other).into_iter().enumerate() {
                        path.push((at, which));
                        walk(body, path, out);
                        path.pop();
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(program, &mut Vec::new(), &mut out);
    out.sort_by_key(|(cut, _)| *cut);
    out.into_iter().map(|(_, path)| path).collect()
}

/// A construct's bodies, in order.
fn bodies(node: &Node) -> Vec<&[Node]> {
    match node {
        Node::Block { body, .. } | Node::Loop { body, .. } | Node::While { body, .. } | Node::For { body, .. } => {
            vec![body]
        }
        Node::If(branches) => branches.iter().map(|(_, body)| body.as_slice()).collect(),
        Node::Match { arms, .. } => arms.iter().map(|(_, body)| body.as_slice()).collect(),
        Node::Stmt(_) | Node::Cut(_) => Vec::new(),
    }
}

/// What the step functions are written with.
struct Writer {
    call: Ident,
    call_ty: Type,
    handed: Ident,
}

impl Writer {
    /// A step: restored, then its code; it ends at a cut (`return`) or finishes.
    fn entry(&self, at: usize, made: &Type, restore: &TokenStream, code: &TokenStream) -> TokenStream {
        let (call, call_ty, handed) = (&self.call, &self.call_ty, &self.handed);
        let entry = format_ident!("__entry{}", at);
        quote! {
            #[allow(unused_mut, unused_variables, unused_assignments, unreachable_code, unused_labels, clippy::all, clippy::pedantic)]
            fn #entry(#call: #call_ty, mut #handed: ::pe2_ext::stepped::Handed)
                -> ::pe2_ext::Result<::pe2_ext::stepped::Flow<#made>>
            {
                #restore
                #code
            }
        }
    }

    /// What a step after a cut begins with: what the cut carried, and what it binds.
    fn restore(&self, cut: usize, cuts: &[Cut], carried: &[Bound]) -> TokenStream {
        let handed = &self.handed;
        let idents = carried.iter().map(binding);
        let types = carried.iter().filter_map(|b| b.ty.as_ref());
        let state = (!carried.is_empty()).then(|| quote!(let (#(#idents,)*): (#(#types,)*) = #handed.state()?;));
        let cut = &cuts[cut];
        let (ident, ty, mutable) = (&cut.ident, &cut.ty, cut.mutable.then(|| quote!(mut)));
        let binding = match &cut.kind {
            Kind::Request(_) => quote!(let #mutable #ident: #ty = #handed.requested()?;),
            Kind::Each { .. } | Kind::EachSteps { .. } => quote!(let #mutable #ident: #ty = #handed.joined()?;),
            Kind::Call { .. } => quote!(let #mutable #ident: #ty = #handed.called()?;),
        };
        quote!(#state #binding)
    }

    /// How a step ends at a cut: what it requests or spawns, worked out first, then what it carries - none yet, while
    /// what is carried is worked out - moved on as it is, the same tuple the next step takes (`restore`).
    fn end(cut: usize, cuts: &[Cut], carried: Option<&[Vec<Bound>]>) -> TokenStream {
        let state = carried.map(|carried| carried[cut].iter().map(|b| b.ident.clone()).collect::<Vec<_>>());
        let state = state.unwrap_or_default();
        let state = quote!((#(#state,)*));
        let number = u32::try_from(cut).unwrap_or(u32::MAX);
        let (asked, ended) = match &cuts[cut].kind {
            Kind::Request(pairs) => (
                quote_spanned! {pairs.span()=> let __asked = ::pe2_ext::stepped::needs(#pairs);},
                quote!(::pe2_ext::stepped::Ended::request(#number, #state, __asked)),
            ),
            Kind::Each { items, .. } => (
                quote!(let __asked = ::pe2_ext::stepped::items(#items)?;),
                quote!(::pe2_ext::stepped::Ended::spawn(#number, #state, __asked)),
            ),
            Kind::EachSteps { items, .. } => (
                quote! {
                    let __asked = ::pe2_ext::stepped::items(::core::iter::IntoIterator::into_iter(#items).map(|__item| (__item,)))?;
                },
                quote!(::pe2_ext::stepped::Ended::spawn(#number, #state, __asked)),
            ),
            Kind::Call { args, .. } => (
                quote!(let __asked = ::pe2_ext::stepped::items([(#(#args,)*)])?;),
                quote!(::pe2_ext::stepped::Ended::spawn(#number, #state, __asked)),
            ),
        };
        quote!({ #asked return #ended.map(::pe2_ext::stepped::Flow::Cut); })
    }

    /// A list's code from a place in it, to the next cut or its end: each construct from its start. The body's own last
    /// expression (`top`) finishes the operation. With the cuts it reaches.
    fn code(
        &self,
        list: &[Node],
        from: usize,
        top: bool,
        cuts: &[Cut],
        carried: Option<&[Vec<Bound>]>,
    ) -> (TokenStream, Vec<usize>) {
        let (mut code, mut reached) = (TokenStream::new(), Vec::new());
        let final_at = list.len().saturating_sub(1);
        for (at, node) in list.iter().enumerate().skip(from) {
            match node {
                Node::Stmt(stmt) => match &**stmt {
                    Stmt::Expr(tail, None) if top && at == final_at => {
                        code.extend(quote!(return (#tail).map(::pe2_ext::stepped::Flow::Done);));
                    }
                    stmt => code.extend(quote!(#stmt)),
                },
                Node::Cut(cut) => {
                    code.extend(Self::end(*cut, cuts, carried));
                    reached.push(*cut);
                    return (code, reached);
                }
                construct => {
                    let (written, more) = self.construct(construct, cuts, carried);
                    code.extend(written);
                    reached.extend(more);
                }
            }
        }
        (code, reached)
    }

    /// A construct holding cuts, from its start.
    fn construct(&self, node: &Node, cuts: &[Cut], carried: Option<&[Vec<Bound>]>) -> (TokenStream, Vec<usize>) {
        let mut reached = Vec::new();
        let mut body = |list: &[Node]| {
            let (code, more) = self.code(list, 0, false, cuts, carried);
            reached.extend(more);
            code
        };
        let code = match node {
            Node::Block { label, body: list } => {
                let list = body(list);
                quote!(#label { #list })
            }
            Node::If(branches) => {
                let mut code = TokenStream::new();
                for (at, (cond, list)) in branches.iter().enumerate() {
                    let list = body(list);
                    code.extend(match (at, cond) {
                        (0, Some(cond)) => quote!(if #cond { #list }),
                        (_, Some(cond)) => quote!(else if #cond { #list }),
                        (_, None) => quote!(else { #list }),
                    });
                }
                code
            }
            Node::Match { on, arms } => {
                let arms = arms.iter().map(|(head, list)| {
                    let list = body(list);
                    quote!(#head => { #list })
                });
                quote!(match #on { #(#arms)* })
            }
            Node::Loop { label, body: list } => {
                let list = body(list);
                quote!(#label loop { #list })
            }
            Node::While { label, cond, body: list } => {
                let list = body(list);
                quote!(#label while #cond { #list })
            }
            Node::For { label, pat, items, index, body: list } => {
                let list = body(list);
                quote! {{
                    let mut #index: usize = 0;
                    #label while #index < #items.len() {
                        let #pat = ::core::clone::Clone::clone(&#items[#index]);
                        #index += 1;
                        #list
                    }
                }}
            }
            Node::Stmt(_) | Node::Cut(_) => TokenStream::new(),
        };
        (code, reached)
    }

    /// The code after a cut: the rest of its list, then of each list holding it out to the body's, each construct around
    /// it built again - a loop's first turn the rest of the turn the cut was in. With the cuts it reaches.
    fn after(
        &self,
        program: &[Node],
        path: &Path,
        cuts: &[Cut],
        carried: Option<&[Vec<Bound>]>,
    ) -> (TokenStream, Vec<usize>) {
        // The lists down to the cut, and the construct holding each but the body's.
        let mut lists: Vec<&[Node]> = vec![program];
        let mut holders: Vec<&Node> = Vec::new();
        for &(at, which) in &path[..path.len() - 1] {
            let holder = &lists[lists.len() - 1][at];
            holders.push(holder);
            lists.push(bodies(holder)[which]);
        }
        let depth = lists.len() - 1;
        let (mut code, mut reached) = self.code(lists[depth], path[depth].0 + 1, depth == 0, cuts, carried);
        for level in (0..depth).rev() {
            let holder = holders[level];
            // A loop's interrupted turn runs first, apart: its `break` leaves the loop, its `continue` goes on into it.
            let (exit, first) = (turn_label("exit", level), turn_label("first", level));
            let wrapped = match holder {
                Node::Block { label, .. } => quote!(#label { #code }),
                Node::If(_) | Node::Match { .. } => quote!({ #code }),
                Node::Loop { label, body } => {
                    let (list, more) = self.code(body, 0, false, cuts, carried);
                    reached.extend(more);
                    let turn = retargeted(&code, label.as_ref(), &exit, &first);
                    quote!(#exit: { #first: { #turn } #label loop { #list } })
                }
                Node::While { label, cond, body } => {
                    let (list, more) = self.code(body, 0, false, cuts, carried);
                    reached.extend(more);
                    let turn = retargeted(&code, label.as_ref(), &exit, &first);
                    quote!(#exit: { #first: { #turn } #label while #cond { #list } })
                }
                Node::For { label, pat, items, index, body } => {
                    let (list, more) = self.code(body, 0, false, cuts, carried);
                    reached.extend(more);
                    let turn = retargeted(&code, label.as_ref(), &exit, &first);
                    quote! {
                        #exit: {
                            #first: {
                                let #pat = ::core::clone::Clone::clone(&#items[#index - 1]);
                                #turn
                            }
                            #label while #index < #items.len() {
                                let #pat = ::core::clone::Clone::clone(&#items[#index]);
                                #index += 1;
                                #list
                            }
                        }
                    }
                }
                Node::Stmt(_) | Node::Cut(_) => code,
            };
            let (rest, more) = self.code(lists[level], path[level].0 + 1, level == 0, cuts, carried);
            reached.extend(more);
            code = quote!(#wrapped #rest);
        }
        (code, reached)
    }
}

/// A label of the blocks a loop's interrupted turn runs in: `exit` around it and the loop, `first` around the turn.
fn turn_label(what: &str, level: usize) -> syn::Lifetime {
    syn::Lifetime::new(&format!("'__{what}{level}"), Span::call_site())
}

/// A loop's interrupted turn, its `break` and `continue` aimed at the blocks it runs in before the loop: those of the loop
/// itself - unlabeled where no loop or closure stands between, or naming its label - leave the loop (`exit`) or go on into
/// it (`first`).
fn retargeted(turn: &TokenStream, label: Option<&Label>, exit: &syn::Lifetime, first: &syn::Lifetime) -> TokenStream {
    struct Retarget<'a> {
        own: Option<String>,
        exit: &'a syn::Lifetime,
        first: &'a syn::Lifetime,
        depth: usize,
    }
    impl Retarget<'_> {
        fn ours(&self, label: Option<&syn::Lifetime>) -> bool {
            match label {
                None => self.depth == 0,
                Some(label) => self.own.as_deref() == Some(label.ident.to_string().as_str()),
            }
        }
    }
    impl VisitMut for Retarget<'_> {
        fn visit_expr_mut(&mut self, expr: &mut Expr) {
            match expr {
                Expr::Break(gone) if self.ours(gone.label.as_ref()) => gone.label = Some(self.exit.clone()),
                Expr::Continue(went) if self.ours(went.label.as_ref()) => {
                    let first = self.first;
                    *expr = syn::parse_quote!(break #first);
                }
                Expr::Loop(_) | Expr::While(_) | Expr::ForLoop(_) | Expr::Closure(_) | Expr::Async(_) => {
                    self.depth += 1;
                    visit_mut::visit_expr_mut(self, expr);
                    self.depth -= 1;
                }
                _ => visit_mut::visit_expr_mut(self, expr),
            }
        }
        fn visit_item_mut(&mut self, _: &mut syn::Item) {}
    }
    let Ok(mut block) = syn::parse2::<syn::Block>(quote!({ #turn })) else { return turn.clone() };
    let own = label.map(|label| label.name.ident.to_string());
    Retarget { own, exit, first, depth: 0 }.visit_block_mut(&mut block);
    let stmts = &block.stmts;
    quote!(#(#stmts)*)
}

/// What each cut carries: what is bound where it stands - its arguments first - and used on any way on from it, or by
/// what a cut it reaches carries, each with its type, in the order bound.
fn carried(
    writer: &Writer,
    program: &[Node],
    cuts: &[Cut],
    paths: &[Path],
    params: &[Bound],
) -> syn::Result<Vec<Vec<Bound>>> {
    let scopes: Vec<Vec<Bound>> = paths.iter().map(|path| scope(program, cuts, path, params)).collect();
    let mut uses: Vec<(Vec<String>, Vec<usize>)> = Vec::new();
    for path in paths {
        let (code, reached) = writer.after(program, path, cuts, None);
        let mut named = Vec::new();
        names(code, &mut named);
        uses.push((named, reached));
    }
    let mut carried: Vec<Vec<String>> = vec![Vec::new(); cuts.len()];
    // What a cut reaches carries, it carries too, where it binds it: until nothing more is carried.
    loop {
        let mut more = false;
        for cut in 0..cuts.len() {
            let (named, reached) = &uses[cut];
            let wanted: Vec<String> = scopes[cut]
                .iter()
                .filter(|b| b.ident != cuts[cut].ident && !carried[cut].contains(&b.ident.to_string()))
                .filter(|b| {
                    named.iter().any(|n| b.ident == n)
                        || reached.iter().any(|other| carried[*other].iter().any(|n| b.ident == n))
                })
                .map(|b| b.ident.to_string())
                .collect();
            more |= !wanted.is_empty();
            for name in wanted {
                if !carried[cut].contains(&name) {
                    carried[cut].push(name);
                }
            }
        }
        if !more {
            break;
        }
    }
    let mut out = Vec::new();
    for (cut, names) in carried.iter().enumerate() {
        let mut live: Vec<Bound> = Vec::new();
        for b in scopes[cut].iter().rev() {
            if !names.contains(&b.ident.to_string()) {
                continue;
            }
            if let Some(other) = live.iter().find(|l| l.ident == b.ident) {
                if other.depth != b.depth {
                    let what =
                        format!("`{}` is bound twice around a cut, one shadowing the other: rename one", b.ident);
                    return Err(error(b.ident.span(), &what));
                }
                continue;
            }
            if b.ty.is_none() {
                let what = format!(
                    "`{0}` is used after a step ends at a cut, so it is carried to the next: write its type (`let {0}: T \
                     = ...`)",
                    b.ident
                );
                return Err(error(b.ident.span(), &what));
            }
            live.push(b.clone());
        }
        live.reverse();
        out.push(live);
    }
    Ok(out)
}

/// What is bound where a cut stands: the arguments, then what each list down to it binds before it, and a `for`'s
/// index and what it holds.
fn scope(program: &[Node], cuts: &[Cut], path: &Path, params: &[Bound]) -> Vec<Bound> {
    let mut bound: Vec<Bound> = params.to_vec();
    let mut list = program;
    for (depth, &(at, which)) in path.iter().enumerate() {
        for node in &list[..at] {
            match node {
                Node::Stmt(stmt) => {
                    if let Stmt::Local(local) = &**stmt {
                        bind(&local.pat, None, depth, &mut bound);
                    }
                }
                Node::Cut(other) => {
                    let other = &cuts[*other];
                    let ty = Some(other.ty.clone());
                    bound.push(Bound { ident: other.ident.clone(), ty, mutable: other.mutable, depth });
                }
                _ => {}
            }
        }
        let holder = &list[at];
        if let Node::For { index, items, .. } = holder {
            let ty = Some(syn::parse_quote!(usize));
            bound.push(Bound { ident: index.clone(), ty, mutable: true, depth });
            // What it goes over is named on every turn, so carried as bound before.
            let _ = items;
        }
        if let Node::Cut(_) = holder {
            break;
        }
        list = bodies(holder)[which];
    }
    bound
}

/// The variables a pattern binds, with the type where it is written.
fn bind(pat: &Pat, ty: Option<&Type>, depth: usize, out: &mut Vec<Bound>) {
    match pat {
        Pat::Ident(ident) => {
            let mutable = ident.mutability.is_some();
            out.push(Bound { ident: ident.ident.clone(), ty: ty.cloned(), mutable, depth });
        }
        Pat::Type(typed) => bind(&typed.pat, Some(&typed.ty), depth, out),
        Pat::Tuple(tuple) => tuple.elems.iter().for_each(|pat| bind(pat, None, depth, out)),
        Pat::TupleStruct(tuple) => tuple.elems.iter().for_each(|pat| bind(pat, None, depth, out)),
        Pat::Struct(fields) => fields.fields.iter().for_each(|field| bind(&field.pat, None, depth, out)),
        Pat::Reference(reference) => bind(&reference.pat, None, depth, out),
        Pat::Slice(slice) => slice.elems.iter().for_each(|pat| bind(pat, None, depth, out)),
        _ => {}
    }
}

/// The names code uses: its identifiers but fields and methods (after a `.`), and those a format string names
/// (`"{file}"`).
fn names(tokens: TokenStream, out: &mut Vec<String>) {
    let mut after_dot = false;
    for token in tokens {
        match &token {
            TokenTree::Ident(ident) => {
                if !after_dot {
                    out.push(ident.to_string());
                }
                after_dot = false;
            }
            TokenTree::Punct(punct) => after_dot = punct.as_char() == '.',
            TokenTree::Group(group) => {
                names(group.stream(), out);
                after_dot = false;
            }
            TokenTree::Literal(literal) => {
                let text = literal.to_string();
                for part in text.split('{').skip(1) {
                    let name: String = part.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    if !name.is_empty() && part[name.len()..].starts_with(['}', ':']) {
                        out.push(name);
                    }
                }
                after_dot = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::expand;

    fn expanded(code: &str) -> String {
        expand(&syn::parse_str(code).expect("a function")).to_string()
    }

    #[test]
    fn a_body_is_cut_where_it_requests_and_spawns() {
        let code = expanded(
            "fn op(call: &mut Call) -> Result<u32> {
                let first: String = call.given().to_string();
                #[request]
                let found: Vec<Table> = call.tables_of([(first.as_str(), \"native\")])?;
                #[each]
                let sizes: Vec<u32> = found.into_iter().map(|table: Table| table.len() as u32).collect();
                Ok(sizes.into_iter().sum::<u32>() + first.len() as u32)
            }",
        );
        for part in ["fn __entry0", "fn __entry1", "fn __entry2", "fn __each1", ". cut () . cut_each (__each1)"] {
            assert!(code.contains(part), "{part}: {code}");
        }
        // `first` lives to the end, carried twice; `found` only into the spawn.
        assert_eq!(code.matches("let (first ,) : (String ,) = __handed . state () ?").count(), 2, "{code}");
    }

    #[test]
    fn a_cut_in_a_branch_goes_on_after_the_branch() {
        let code = expanded(
            "fn op(call: &mut Call) -> Result<u32> {
                let wanted: bool = true;
                let mut size: u32 = 0;
                if wanted {
                    #[request]
                    let found: Table = call.table_of(\"a\", \"native\")?;
                    size = found.len() as u32;
                }
                Ok(size)
            }",
        );
        // After the cut: the rest of the branch, then what follows the `if`.
        assert!(code.contains("{ size = found . len () as u32 ; } return (Ok (size))"), "{code}");
    }

    #[test]
    fn a_cut_in_a_for_goes_on_from_its_item() {
        let code = expanded(
            "fn op(call: &mut Call) -> Result<u32> {
                let names: Vec<String> = vec![];
                let mut total: u32 = 0;
                for name in names {
                    #[request]
                    let found: Table = call.table_of(&name, \"native\")?;
                    total += found.len() as u32;
                }
                Ok(total)
            }",
        );
        assert!(code.contains("__index0 : usize"), "{code}");
        assert!(code.contains("names [__index0 - 1]"), "{code}");
    }

    #[test]
    fn a_helper_is_called_with_its_arguments() {
        let code = expanded(
            "fn op(call: &mut Call, node: String, depth: u32) -> Result<u32> {
                #[call]
                let below: u32 = op(call, node.clone(), depth + 1)?;
                Ok(below + depth)
            }",
        );
        assert!(code.contains("let (node , depth ,) : (String , u32 ,) = __handed . item () ?"), "{code}");
        assert!(code.contains(". cut_steps (op)"), "{code}");
        assert!(code.contains("let below : u32 = __handed . called () ?"), "{code}");
    }

    #[test]
    fn what_is_carried_says_its_type() {
        let code = expanded(
            "fn op(call: &mut Call) -> Result<u32> {
                let first = 1;
                #[request]
                let found: Vec<Table> = call.tables_of([(\"a\", \"native\")])?;
                Ok(first)
            }",
        );
        assert!(code.contains("`first` is used after a step ends at a cut"), "{code}");
    }

    #[test]
    fn a_cut_in_an_expression_is_refused() {
        let code = expanded(
            "fn op(call: &mut Call) -> Result<u32> {
                let size = {
                    #[request]
                    let found: Vec<Table> = call.tables_of([(\"a\", \"native\")])?;
                    found.len()
                };
                Ok(size as u32)
            }",
        );
        assert!(code.contains("a cut stands where a statement does"), "{code}");
    }
}
