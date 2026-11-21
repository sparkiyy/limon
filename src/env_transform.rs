use crate::config::{EnvConfig, EnvValueType};
use full_moon::ast::luau::{ConstAssignment, IfExpression};
use full_moon::ast::{
    Assignment, BinOp, Block, Call, CompoundAssignment, Expression, Field, FunctionArgs,
    FunctionBody, FunctionCall, FunctionDeclaration, GenericFor, If, Index, LastStmt,
    LocalAssignment, NumericFor, Prefix, Repeat, Stmt, Suffix, TableConstructor, UnOp, Var, While,
};
use full_moon::node::Node;
use full_moon::tokenizer::{StringLiteralQuoteType, Symbol, TokenReference, TokenType};
use full_moon::visitors::{Visit, Visitor};
use std::collections::{HashMap, HashSet};

/// A compile-time known Luau primitive value derived from environment variables or literals.
#[derive(Debug, Clone, PartialEq)]
pub enum StaticValue {
    Nil,
    Bool(bool),
    Number(f64),
    String(String),
}

impl StaticValue {
    /// Returns Luau truthiness (`false` and `nil` are falsy; everything else including `0` and `""` is truthy).
    pub fn is_truthy(&self) -> bool {
        !matches!(self, StaticValue::Nil | StaticValue::Bool(false))
    }

    /// Luau `==` equality semantics.
    pub fn luau_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (StaticValue::Nil, StaticValue::Nil) => true,
            (StaticValue::Bool(a), StaticValue::Bool(b)) => a == b,
            (StaticValue::Number(a), StaticValue::Number(b)) => a == b,
            (StaticValue::String(a), StaticValue::String(b)) => a == b,
            _ => false,
        }
    }

    /// Parses an OS environment variable string into a `StaticValue` according to `ty`.
    pub fn from_env_str(name: &str, raw: &str, ty: EnvValueType) -> Result<Self, String> {
        match ty {
            EnvValueType::String => Ok(StaticValue::String(raw.to_string())),
            EnvValueType::Bool => match raw.trim().to_ascii_lowercase().as_str() {
                "true" | "1" => Ok(StaticValue::Bool(true)),
                "false" | "0" => Ok(StaticValue::Bool(false)),
                _ => Err(format!(
                    "Environment variable '{}' has value {:?}, which cannot be cast to bool (expected true/false/1/0).",
                    name, raw
                )),
            },
            EnvValueType::Number => parse_explicit_number(raw.trim())
                .map(StaticValue::Number)
                .ok_or_else(|| {
                    format!(
                        "Environment variable '{}' has value {:?}, which cannot be cast to a finite number.",
                        name, raw
                    )
                }),
            EnvValueType::Auto => Ok(auto_cast_env_str(raw)),
        }
    }

    /// Converts a validated `toml::Value` default into a `StaticValue`.
    pub fn from_toml_value(name: &str, val: &toml::Value) -> Result<Self, String> {
        match val {
            toml::Value::Boolean(b) => Ok(StaticValue::Bool(*b)),
            toml::Value::Integer(i) => Ok(StaticValue::Number(*i as f64)),
            toml::Value::Float(f) if f.is_finite() => Ok(StaticValue::Number(*f)),
            toml::Value::Float(_) => Err(format!(
                "Invalid [env.defaults] value for '{}': non-finite float is not supported.",
                name
            )),
            toml::Value::String(s) => Ok(StaticValue::String(s.clone())),
            other => Err(format!(
                "Invalid [env.defaults] value for '{}': expected string, boolean, or number, got {}.",
                name,
                other.type_str()
            )),
        }
    }

    /// Formats this value as a single-line Luau literal.
    pub fn to_luau_literal(&self) -> String {
        match self {
            StaticValue::Nil => "nil".to_string(),
            StaticValue::Bool(true) => "true".to_string(),
            StaticValue::Bool(false) => "false".to_string(),
            StaticValue::Number(n) => {
                if *n == 0.0 {
                    "0".to_string()
                } else if n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_992.0 {
                    format!("{}", *n as i64)
                } else {
                    format!("{}", n)
                }
            }
            StaticValue::String(s) => format_luau_string_literal(s),
        }
    }
}

fn auto_cast_env_str(raw: &str) -> StaticValue {
    match raw {
        "true" => StaticValue::Bool(true),
        "false" => StaticValue::Bool(false),
        "nil" => StaticValue::Nil,
        _ => {
            if is_auto_cast_number(raw)
                && let Ok(n) = raw.parse::<f64>()
                && n.is_finite()
            {
                return StaticValue::Number(n);
            }
            StaticValue::String(raw.to_string())
        }
    }
}

/// Strict JSON-like finite number grammar: `^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?$`
/// Keeps padded IDs like `"007"`, `""`, `"NaN"`, `"inf"`, and `" 1 "` as strings.
fn is_auto_cast_number(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    if bytes.get(i) == Some(&b'-') {
        i += 1;
    }
    if i >= bytes.len() {
        return false;
    }
    if bytes[i] == b'0' {
        i += 1;
        if bytes.get(i).is_some_and(|b| b.is_ascii_digit()) {
            return false;
        }
    } else if bytes[i].is_ascii_digit() {
        while bytes.get(i).is_some_and(|b| b.is_ascii_digit()) {
            i += 1;
        }
    } else {
        return false;
    }

    if bytes.get(i) == Some(&b'.') {
        i += 1;
        if !bytes.get(i).is_some_and(|b| b.is_ascii_digit()) {
            return false;
        }
        while bytes.get(i).is_some_and(|b| b.is_ascii_digit()) {
            i += 1;
        }
    }

    if bytes.get(i).is_some_and(|b| *b == b'e' || *b == b'E') {
        i += 1;
        if bytes.get(i).is_some_and(|b| *b == b'+' || *b == b'-') {
            i += 1;
        }
        if !bytes.get(i).is_some_and(|b| b.is_ascii_digit()) {
            return false;
        }
        while bytes.get(i).is_some_and(|b| b.is_ascii_digit()) {
            i += 1;
        }
    }

    i == bytes.len()
}

fn parse_explicit_number(s: &str) -> Option<f64> {
    if s.is_empty() {
        return None;
    }
    let (neg, rest) = if let Some(r) = s.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = s.strip_prefix('+') {
        (false, r)
    } else {
        (false, s)
    };
    let val = parse_luau_unsigned_number(rest)?;
    let signed = if neg { -val } else { val };
    if signed.is_finite() {
        Some(signed)
    } else {
        None
    }
}

fn parse_luau_unsigned_number(raw: &str) -> Option<f64> {
    if raw.is_empty() {
        return None;
    }
    let cleaned: String = raw.chars().filter(|c| *c != '_').collect();
    if cleaned.is_empty() {
        return None;
    }

    if let Some(hex) = cleaned.strip_prefix("0x").or_else(|| cleaned.strip_prefix("0X")) {
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        return u64::from_str_radix(hex, 16).ok().map(|v| v as f64);
    }

    if let Some(bin) = cleaned.strip_prefix("0b").or_else(|| cleaned.strip_prefix("0B")) {
        if bin.is_empty() || !bin.bytes().all(|b| b == b'0' || b == b'1') {
            return None;
        }
        return u64::from_str_radix(bin, 2).ok().map(|v| v as f64);
    }

    // Reject non-numeric f64 literals like "inf", "infinity", "nan"
    let first = cleaned.as_bytes()[0];
    if !first.is_ascii_digit() && first != b'.' {
        return None;
    }
    let parsed: f64 = cleaned.parse().ok()?;
    if parsed.is_finite() {
        Some(parsed)
    } else {
        None
    }
}

fn format_luau_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7F => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Pre-validated and pre-cast environment bindings for a project run.
pub struct ResolvedEnv<'a> {
    config: &'a EnvConfig,
    env_vars: &'a HashMap<String, String>,
    typed_values: HashMap<String, StaticValue>,
    default_values: HashMap<String, StaticValue>,
}

impl<'a> ResolvedEnv<'a> {
    /// Validates `[env]` configuration and eagerly checks any explicit `[env.types]` casts
    /// against the provided `env_vars`.
    pub fn new(config: &'a EnvConfig, env_vars: &'a HashMap<String, String>) -> Result<Self, String> {
        config.validate()?;

        let missing: Vec<&str> = config
            .required
            .iter()
            .filter(|name| !env_vars.contains_key(name.as_str()))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "Missing required environment variable(s): {}",
                missing.join(", ")
            ));
        }

        let mut default_values = HashMap::with_capacity(config.defaults.len());
        for (name, val) in &config.defaults {
            default_values.insert(name.clone(), StaticValue::from_toml_value(name, val)?);
        }

        let mut typed_values = HashMap::with_capacity(config.types.len());
        for (name, ty) in &config.types {
            if let Some(raw) = env_vars.get(name) {
                typed_values.insert(name.clone(), StaticValue::from_env_str(name, raw, *ty)?);
            }
        }

        Ok(Self {
            config,
            env_vars,
            typed_values,
            default_values,
        })
    }

    /// Resolves a variable name referenced via `_G.<name>`.
    /// Returns `None` when the variable should be left untouched in source.
    pub fn resolve(&self, name: &str) -> Option<StaticValue> {
        if let Some(v) = self.typed_values.get(name) {
            return Some(v.clone());
        }
        if let Some(raw) = self.env_vars.get(name) {
            let ty = if self.config.cast {
                EnvValueType::Auto
            } else {
                EnvValueType::String
            };
            // Auto and String never fail
            return StaticValue::from_env_str(name, raw, ty).ok();
        }
        if let Some(v) = self.default_values.get(name) {
            return Some(v.clone());
        }
        if self.config.types.contains_key(name) {
            return Some(StaticValue::Nil);
        }
        None
    }
}

#[derive(Debug, Clone)]
struct SpanEdit {
    start: usize,
    end: usize,
    replacement: String,
}

fn apply_edits(code: &str, mut edits: Vec<SpanEdit>) -> String {
    if edits.is_empty() {
        return code.to_string();
    }
    edits.sort_by_key(|e| (e.start, e.end));

    let mut out = String::with_capacity(code.len());
    let mut cursor = 0usize;
    for edit in edits {
        debug_assert!(
            edit.start >= cursor && edit.end >= edit.start && edit.end <= code.len(),
            "Overlapping or out-of-bounds SpanEdit: cursor={}, edit={}..{}",
            cursor,
            edit.start,
            edit.end
        );
        if edit.start < cursor {
            continue;
        }
        out.push_str(&code[cursor..edit.start]);
        out.push_str(&edit.replacement);
        cursor = edit.end;
    }
    out.push_str(&code[cursor..]);
    out
}

fn line_start_of(code: &str, pos: usize) -> usize {
    match code[..pos].rfind('\n') {
        Some(idx) => idx + 1,
        None => 0,
    }
}

fn only_horiz_ws(slice: &str) -> bool {
    slice.bytes().all(|b| b == b' ' || b == b'\t')
}

fn extend_start_if_line_prefix_blank(code: &str, pos: usize) -> usize {
    let ls = line_start_of(code, pos);
    if only_horiz_ws(&code[ls..pos]) {
        ls
    } else {
        pos
    }
}

fn extend_end_horiz_if_eol(code: &str, pos: usize) -> usize {
    let bytes = code.as_bytes();
    let mut i = pos;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i == bytes.len() || bytes[i] == b'\n' || bytes[i] == b'\r' {
        i
    } else {
        pos
    }
}

fn extend_end_through_line_if_blank(code: &str, pos: usize) -> usize {
    let bytes = code.as_bytes();
    let mut i = pos;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i == bytes.len() {
        return i;
    }
    if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
        return i + 2;
    }
    if bytes[i] == b'\n' {
        return i + 1;
    }
    pos
}

fn leading_horiz_ws(code: &str, line_start: usize) -> &str {
    let bytes = code.as_bytes();
    let mut i = line_start;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    &code[line_start..i]
}

fn newlines_only(slice: &str) -> String {
    let mut out = String::new();
    let bytes = slice.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            out.push_str("\r\n");
            i += 2;
        } else if bytes[i] == b'\n' {
            out.push('\n');
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

fn blank_span_text(code: &str, start: usize, end: usize, preserve_lines: bool) -> String {
    if preserve_lines {
        newlines_only(&code[start..end])
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ExprContext {
    needs_parens_for_non_atomic: bool,
    is_power_lhs: bool,
}

#[derive(Debug, Clone, Default)]
struct EvalResult {
    value: Option<StaticValue>,
    has_env: bool,
}

impl EvalResult {
    fn unknown() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchState {
    AlwaysTrue,
    AlwaysFalse,
    Unknown,
}

/// Collects all variable names assigned via `x = ...`, `x += ...`, or `function x() end`.
#[derive(Default)]
struct MutationCollector {
    mutated: HashSet<String>,
}

impl Visitor for MutationCollector {
    fn visit_assignment(&mut self, node: &Assignment) {
        for var in node.variables().iter() {
            if let Var::Name(tok) = var
                && let Some(name) = identifier_name(tok)
            {
                self.mutated.insert(name.to_string());
            }
        }
    }

    fn visit_compound_assignment(&mut self, node: &CompoundAssignment) {
        if let Var::Name(tok) = node.lhs()
            && let Some(name) = identifier_name(tok)
        {
            self.mutated.insert(name.to_string());
        }
    }

    fn visit_function_declaration(&mut self, node: &FunctionDeclaration) {
        let name = node.name();
        if name.method_name().is_none()
            && name.names().len() == 1
            && let Some(tok) = name.names().iter().next()
            && let Some(ident) = identifier_name(tok)
        {
            self.mutated.insert(ident.to_string());
        }
    }
}

/// Checks whether any identifier token in a subtree belongs to `names`.
struct IdentifierFinder<'a> {
    names: &'a HashSet<String>,
    found: bool,
}

impl<'a> Visitor for IdentifierFinder<'a> {
    fn visit_identifier(&mut self, token: &full_moon::tokenizer::Token) {
        if self.found {
            return;
        }
        if let TokenType::Identifier { identifier } = token.token_type()
            && self.names.contains(identifier.as_str())
        {
            self.found = true;
        }
    }
}

fn identifier_name(tok: &TokenReference) -> Option<&str> {
    match tok.token().token_type() {
        TokenType::Identifier { identifier } => Some(identifier.as_str()),
        _ => None,
    }
}

fn extract_string_literal(tok: &TokenReference) -> Option<String> {
    match tok.token().token_type() {
        TokenType::StringLiteral {
            literal,
            quote_type,
            ..
        } => unescape_luau_string(literal.as_str(), *quote_type),
        _ => None,
    }
}

fn unescape_luau_string(raw: &str, quote_type: StringLiteralQuoteType) -> Option<String> {
    if quote_type == StringLiteralQuoteType::Brackets {
        let trimmed = if let Some(rest) = raw.strip_prefix("\r\n") {
            rest
        } else if let Some(rest) = raw.strip_prefix('\n') {
            rest
        } else {
            raw
        };
        return Some(trimmed.to_string());
    }

    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let esc = chars.next()?;
        match esc {
            'a' => out.push('\u{0007}'),
            'b' => out.push('\u{0008}'),
            'f' => out.push('\u{000C}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\u{000B}'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            '\n' => out.push('\n'),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            'z' => {
                while chars.peek().is_some_and(|ch| ch.is_ascii_whitespace()) {
                    chars.next();
                }
            }
            'x' => {
                let h1 = chars.next()?;
                let h2 = chars.next()?;
                let mut hex = String::with_capacity(2);
                hex.push(h1);
                hex.push(h2);
                let byte = u8::from_str_radix(&hex, 16).ok()?;
                out.push(byte as char);
            }
            'u' => {
                if chars.next() != Some('{') {
                    return None;
                }
                let mut hex = String::new();
                loop {
                    let ch = chars.next()?;
                    if ch == '}' {
                        break;
                    }
                    hex.push(ch);
                }
                let code = u32::from_str_radix(&hex, 16).ok()?;
                out.push(char::from_u32(code)?);
            }
            d if d.is_ascii_digit() => {
                let mut val = d.to_digit(10)?;
                for _ in 0..2 {
                    if let Some(&next_d) = chars.peek() {
                        if let Some(digit) = next_d.to_digit(10) {
                            val = val * 10 + digit;
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                if val > 255 {
                    return None;
                }
                out.push(val as u8 as char);
            }
            other => out.push(other),
        }
    }
    Some(out)
}

fn is_atomic_expr(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::Number(_)
            | Expression::String(_)
            | Expression::Symbol(_)
            | Expression::Parentheses { .. }
            | Expression::TableConstructor(_)
            | Expression::FunctionCall(_)
            | Expression::Var(_)
            | Expression::InterpolatedString(_)
    )
}

fn first_token_is_left_paren(node: &impl Node) -> bool {
    node.tokens()
        .next()
        .is_some_and(|tok| tok.is_symbol(Symbol::LeftParen))
}

fn collect_top_level_declared_names(block: &Block) -> HashSet<String> {
    let mut names = HashSet::new();
    for stmt in block.stmts() {
        match stmt {
            Stmt::LocalAssignment(la) => {
                for tok in la.names().iter() {
                    if let Some(n) = identifier_name(tok) {
                        names.insert(n.to_string());
                    }
                }
            }
            Stmt::ConstAssignment(ca) => {
                for tok in ca.names().iter() {
                    if let Some(n) = identifier_name(tok) {
                        names.insert(n.to_string());
                    }
                }
            }
            Stmt::LocalFunction(lf) => {
                if let Some(n) = identifier_name(lf.name()) {
                    names.insert(n.to_string());
                }
            }
            Stmt::ConstFunction(cf) => {
                if let Some(n) = identifier_name(cf.name()) {
                    names.insert(n.to_string());
                }
            }
            Stmt::TypeDeclaration(td) => {
                if let Some(n) = identifier_name(td.type_name()) {
                    names.insert(n.to_string());
                }
            }
            _ => {}
        }
    }
    names
}

fn block_last_stmt_has_no_semi(block: &Block) -> bool {
    if let Some((_, semi)) = block.last_stmt_with_semicolon() {
        return semi.is_none();
    }
    block
        .stmts_with_semicolon()
        .last()
        .is_some_and(|(_, semi)| semi.is_none())
}

fn needs_do_wrapper(
    winning_block: &Block,
    remaining_stmts: &[(Stmt, Option<TokenReference>)],
    enclosing_last: Option<&(LastStmt, Option<TokenReference>)>,
    prev_stmt_has_no_semi: bool,
) -> bool {
    let has_following = !remaining_stmts.is_empty() || enclosing_last.is_some();

    if winning_block.last_stmt().is_some() && has_following {
        return true;
    }

    if prev_stmt_has_no_semi && first_token_is_left_paren(winning_block) {
        return true;
    }

    if block_last_stmt_has_no_semi(winning_block)
        && remaining_stmts.iter().any(|(s, _)| first_token_is_left_paren(s))
    {
        return true;
    }

    if has_following {
        let declared = collect_top_level_declared_names(winning_block);
        if !declared.is_empty() {
            let mut finder = IdentifierFinder {
                names: &declared,
                found: false,
            };
            for (stmt, _) in remaining_stmts {
                stmt.visit(&mut finder);
                if finder.found {
                    return true;
                }
            }
            if let Some((last, _)) = enclosing_last {
                last.visit(&mut finder);
                if finder.found {
                    return true;
                }
            }
        }
    }

    false
}

struct EnvTransformer<'a> {
    code: &'a str,
    config: &'a EnvConfig,
    resolved: &'a ResolvedEnv<'a>,
    mutated_names: HashSet<String>,
    scopes: Vec<HashMap<String, Option<StaticValue>>>,
    edits: Vec<SpanEdit>,
}

impl<'a> EnvTransformer<'a> {
    fn new(code: &'a str, config: &'a EnvConfig, resolved: &'a ResolvedEnv<'a>) -> Self {
        Self {
            code,
            config,
            resolved,
            mutated_names: HashSet::new(),
            scopes: vec![HashMap::new()],
            edits: Vec::new(),
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind_local(&mut self, name: String, value: Option<StaticValue>) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, value);
        }
    }

    fn lookup_local(&self, name: &str) -> Option<StaticValue> {
        for scope in self.scopes.iter().rev() {
            if let Some(entry) = scope.get(name) {
                return entry.clone();
            }
        }
        None
    }

    fn format_literal_at_span(
        &self,
        val: &StaticValue,
        start: usize,
        end: usize,
        ctx: ExprContext,
        extra_newlines: &str,
    ) -> String {
        let mut lit = val.to_luau_literal();
        if matches!(val, StaticValue::Number(n) if *n < 0.0)
            && (ctx.is_power_lhs || (start > 0 && self.code.as_bytes()[start - 1] == b'-'))
        {
            lit = format!("({lit})");
        }

        if extra_newlines.is_empty() {
            let next_byte = self.code.as_bytes().get(end).copied();
            let needs_space = (matches!(val, StaticValue::Number(_)) && next_byte == Some(b'.'))
                || (matches!(
                    val,
                    StaticValue::Nil | StaticValue::Bool(_) | StaticValue::Number(_)
                ) && next_byte.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_'));
            if needs_space {
                lit.push(' ');
            }
        } else {
            lit.push_str(extra_newlines);
        }

        lit
    }

    fn match_env_var(&self, expr: &Expression) -> Option<(String, usize, usize)> {
        let Expression::Var(Var::Expression(ve)) = expr else {
            return None;
        };
        let Prefix::Name(prefix_tok) = ve.prefix() else {
            return None;
        };
        if identifier_name(prefix_tok) != Some("_G") {
            return None;
        }
        let mut suffixes = ve.suffixes();
        let first_suffix = suffixes.next()?;
        if suffixes.next().is_some() {
            return None;
        }

        let var_name = match first_suffix {
            Suffix::Index(Index::Dot { name, .. }) => identifier_name(name)?.to_string(),
            Suffix::Index(Index::Brackets { expression, .. }) => match &**expression {
                Expression::String(tok) => extract_string_literal(tok)?,
                _ => return None,
            },
            _ => return None,
        };

        let start = ve.start_position()?.bytes();
        let end = ve.end_position()?.bytes();
        Some((var_name, start, end))
    }

    fn transform_ast(&mut self, ast: &full_moon::ast::Ast) {
        let mut collector = MutationCollector::default();
        collector.visit_ast(ast);
        self.mutated_names = collector.mutated;
        self.transform_block(ast.nodes(), false);
    }

    fn transform_block(&mut self, block: &Block, create_scope: bool) {
        if create_scope {
            self.push_scope();
        }

        let stmts: Vec<_> = block.stmts_with_semicolon().collect();
        let mut effective_prev_no_semi = false;
        for i in 0..stmts.len() {
            let (stmt, semi) = stmts[i];
            let remaining: Vec<(Stmt, Option<TokenReference>)> = stmts[i + 1..]
                .iter()
                .map(|(s, sm)| ((*s).clone(), (*sm).clone()))
                .collect();

            match stmt {
                Stmt::If(if_stmt) => {
                    effective_prev_no_semi = self.transform_if_stmt(
                        if_stmt,
                        semi.as_ref(),
                        &remaining,
                        block.last_stmt_with_semicolon(),
                        effective_prev_no_semi,
                    );
                }
                Stmt::While(while_stmt) => {
                    let removed = self.transform_while_stmt(while_stmt, semi.as_ref());
                    if !removed {
                        effective_prev_no_semi = semi.is_none();
                    }
                }
                other => {
                    self.transform_other_stmt(other);
                    effective_prev_no_semi = semi.is_none();
                }
            }
        }

        if let Some((LastStmt::Return(ret), _)) = block.last_stmt_with_semicolon() {
            for expr in ret.returns().iter() {
                self.eval_expr(expr, ExprContext::default());
            }
        }

        if create_scope {
            self.pop_scope();
        }
    }

    fn transform_other_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::LocalAssignment(la) => self.transform_local_assignment(la),
            Stmt::ConstAssignment(ca) => self.transform_const_assignment(ca),
            Stmt::Assignment(assign) => {
                for var in assign.variables().iter() {
                    self.transform_lhs_var(var);
                }
                for expr in assign.expressions().iter() {
                    self.eval_expr(expr, ExprContext::default());
                }
            }
            Stmt::CompoundAssignment(ca) => {
                self.transform_lhs_var(ca.lhs());
                self.eval_expr(ca.rhs(), ExprContext::default());
            }
            Stmt::FunctionCall(fc) => self.transform_function_call(fc),
            Stmt::Do(do_stmt) => self.transform_block(do_stmt.block(), true),
            Stmt::Repeat(rep) => self.transform_repeat_stmt(rep),
            Stmt::NumericFor(nf) => self.transform_numeric_for(nf),
            Stmt::GenericFor(gf) => self.transform_generic_for(gf),
            Stmt::LocalFunction(lf) => {
                if let Some(name) = identifier_name(lf.name()) {
                    self.bind_local(name.to_string(), None);
                }
                self.transform_function_body(lf.body(), false);
            }
            Stmt::ConstFunction(cf) => {
                if let Some(name) = identifier_name(cf.name()) {
                    self.bind_local(name.to_string(), None);
                }
                self.transform_function_body(cf.body(), false);
            }
            Stmt::FunctionDeclaration(fd) => {
                let is_method = fd.name().method_name().is_some();
                if !is_method
                    && fd.name().names().len() == 1
                    && let Some(tok) = fd.name().names().iter().next()
                    && let Some(name) = identifier_name(tok)
                {
                    self.bind_local(name.to_string(), None);
                }
                self.transform_function_body(fd.body(), is_method);
            }
            Stmt::TypeDeclaration(td) => {
                if let Some(name) = identifier_name(td.type_name()) {
                    self.bind_local(name.to_string(), None);
                }
            }
            Stmt::If(_)
            | Stmt::While(_)
            | Stmt::ExportedTypeDeclaration(_)
            | Stmt::ExportedTypeFunction(_)
            | Stmt::TypeFunction(_) => {}
            _ => {}
        }
    }

    fn transform_local_assignment(&mut self, la: &LocalAssignment) {
        let rhs_evals: Vec<EvalResult> = la
            .expressions()
            .iter()
            .map(|expr| self.eval_expr(expr, ExprContext::default()))
            .collect();

        for (i, tok) in la.names().iter().enumerate() {
            let Some(name) = identifier_name(tok) else {
                continue;
            };
            let val = if !self.mutated_names.contains(name) {
                rhs_evals
                    .get(i)
                    .and_then(|res| if res.has_env { res.value.clone() } else { None })
            } else {
                None
            };
            self.bind_local(name.to_string(), val);
        }
    }

    fn transform_const_assignment(&mut self, ca: &ConstAssignment) {
        let rhs_evals: Vec<EvalResult> = ca
            .expressions()
            .iter()
            .map(|expr| self.eval_expr(expr, ExprContext::default()))
            .collect();

        for (i, tok) in ca.names().iter().enumerate() {
            let Some(name) = identifier_name(tok) else {
                continue;
            };
            let val = if !self.mutated_names.contains(name) {
                rhs_evals
                    .get(i)
                    .and_then(|res| if res.has_env { res.value.clone() } else { None })
            } else {
                None
            };
            self.bind_local(name.to_string(), val);
        }
    }

    fn transform_lhs_var(&mut self, var: &Var) {
        if let Var::Expression(ve) = var {
            if let Prefix::Expression(expr) = ve.prefix() {
                self.eval_expr(expr, ExprContext::default());
            }
            for suffix in ve.suffixes() {
                self.transform_suffix(suffix);
            }
        }
    }

    fn transform_repeat_stmt(&mut self, rep: &Repeat) {
        self.push_scope();
        self.transform_block(rep.block(), false);
        self.eval_expr(rep.until(), ExprContext::default());
        self.pop_scope();
    }

    fn transform_numeric_for(&mut self, nf: &NumericFor) {
        self.eval_expr(nf.start(), ExprContext::default());
        self.eval_expr(nf.end(), ExprContext::default());
        if let Some(step) = nf.step() {
            self.eval_expr(step, ExprContext::default());
        }
        self.push_scope();
        if let Some(name) = identifier_name(nf.index_variable()) {
            self.bind_local(name.to_string(), None);
        }
        self.transform_block(nf.block(), false);
        self.pop_scope();
    }

    fn transform_generic_for(&mut self, gf: &GenericFor) {
        for expr in gf.expressions().iter() {
            self.eval_expr(expr, ExprContext::default());
        }
        self.push_scope();
        for tok in gf.names().iter() {
            if let Some(name) = identifier_name(tok) {
                self.bind_local(name.to_string(), None);
            }
        }
        self.transform_block(gf.block(), false);
        self.pop_scope();
    }

    fn transform_function_body(&mut self, body: &FunctionBody, has_implicit_self: bool) {
        self.push_scope();
        if has_implicit_self {
            self.bind_local("self".to_string(), None);
        }
        for param in body.parameters().iter() {
            if let full_moon::ast::Parameter::Name(tok) = param
                && let Some(name) = identifier_name(tok)
            {
                self.bind_local(name.to_string(), None);
            }
        }
        self.transform_block(body.block(), false);
        self.pop_scope();
    }

    fn transform_function_call(&mut self, fc: &FunctionCall) {
        if let Prefix::Expression(expr) = fc.prefix() {
            self.eval_expr(expr, ExprContext::default());
        }
        for suffix in fc.suffixes() {
            self.transform_suffix(suffix);
        }
    }

    fn transform_suffix(&mut self, suffix: &Suffix) {
        match suffix {
            Suffix::Call(Call::AnonymousCall(args)) => self.transform_function_args(args),
            Suffix::Call(Call::MethodCall(mc)) => self.transform_function_args(mc.args()),
            Suffix::Index(Index::Brackets { expression, .. }) => {
                self.eval_expr(expression, ExprContext::default());
            }
            Suffix::Index(Index::Dot { .. }) => {}
            _ => {}
        }
    }

    fn transform_function_args(&mut self, args: &FunctionArgs) {
        match args {
            FunctionArgs::Parentheses { arguments, .. } => {
                for expr in arguments.iter() {
                    self.eval_expr(expr, ExprContext::default());
                }
            }
            FunctionArgs::TableConstructor(tc) => self.transform_table_constructor(tc),
            FunctionArgs::String(_) => {}
            _ => {}
        }
    }

    fn transform_table_constructor(&mut self, tc: &TableConstructor) {
        for field in tc.fields().iter() {
            match field {
                Field::ExpressionKey { key, value, .. } => {
                    self.eval_expr(key, ExprContext::default());
                    self.eval_expr(value, ExprContext::default());
                }
                Field::NameKey { value, .. } => {
                    self.eval_expr(value, ExprContext::default());
                }
                Field::NoKey(expr) => {
                    self.eval_expr(expr, ExprContext::default());
                }
                _ => {}
            }
        }
    }

    fn transform_while_stmt(&mut self, w: &While, semi: Option<&TokenReference>) -> bool {
        let cp = self.edits.len();
        let cond_eval = self.eval_expr(w.condition(), ExprContext::default());
        if self.config.dead_code_elimination
            && cond_eval.has_env
            && cond_eval.value.as_ref().is_some_and(|v| !v.is_truthy())
        {
            self.edits.truncate(cp);
            let raw_start = w.while_token().start_position().unwrap().bytes();
            let raw_end = semi
                .unwrap_or_else(|| w.end_token())
                .end_position()
                .unwrap()
                .bytes();
            self.emit_dead_stmt_removal(raw_start, raw_end);
            return true;
        }
        self.transform_block(w.block(), true);
        false
    }

    fn emit_dead_stmt_removal(&mut self, raw_start: usize, raw_end: usize) {
        let start = extend_start_if_line_prefix_blank(self.code, raw_start);
        if self.config.preserve_lines {
            self.edits.push(SpanEdit {
                start,
                end: raw_end,
                replacement: newlines_only(&self.code[start..raw_end]),
            });
        } else if start == line_start_of(self.code, raw_start) {
            let end = extend_end_through_line_if_blank(self.code, raw_end);
            self.edits.push(SpanEdit {
                start,
                end,
                replacement: String::new(),
            });
        } else {
            self.edits.push(SpanEdit {
                start,
                end: raw_end,
                replacement: String::new(),
            });
        }
    }

    /// Returns the `effective_prev_no_semi` state after this `if` statement.
    fn transform_if_stmt(
        &mut self,
        if_stmt: &If,
        semi: Option<&TokenReference>,
        remaining_stmts: &[(Stmt, Option<TokenReference>)],
        enclosing_last: Option<&(LastStmt, Option<TokenReference>)>,
        prev_stmt_has_no_semi: bool,
    ) -> bool {
        struct BranchEval<'b> {
            kw_tok: &'b TokenReference,
            then_tok: &'b TokenReference,
            has_binding: bool,
            binding_name: Option<String>,
            block: &'b Block,
            state: BranchState,
            cond_edits: Vec<SpanEdit>,
        }

        let mut branches = Vec::new();
        {
            let cp = self.edits.len();
            let eval = self.eval_expr(if_stmt.condition(), ExprContext::default());
            let cond_edits: Vec<SpanEdit> = self.edits.drain(cp..).collect();
            let has_binding = if_stmt.binding().is_some();
            let binding_name = if_stmt
                .binding()
                .and_then(|b| identifier_name(b.name()))
                .map(|s| s.to_string());
            let state = classify_branch(self.config.dead_code_elimination, has_binding, &eval);
            branches.push(BranchEval {
                kw_tok: if_stmt.if_token(),
                then_tok: if_stmt.then_token(),
                has_binding,
                binding_name,
                block: if_stmt.block(),
                state,
                cond_edits,
            });
        }

        if let Some(else_ifs) = if_stmt.else_if() {
            for eif in else_ifs {
                let cp = self.edits.len();
                let eval = self.eval_expr(eif.condition(), ExprContext::default());
                let cond_edits: Vec<SpanEdit> = self.edits.drain(cp..).collect();
                let has_binding = eif.binding().is_some();
                let binding_name = eif
                    .binding()
                    .and_then(|b| identifier_name(b.name()))
                    .map(|s| s.to_string());
                let state = classify_branch(self.config.dead_code_elimination, has_binding, &eval);
                branches.push(BranchEval {
                    kw_tok: eif.else_if_token(),
                    then_tok: eif.then_token(),
                    has_binding,
                    binding_name,
                    block: eif.block(),
                    state,
                    cond_edits,
                });
            }
        }

        let any_known = branches.iter().any(|b| b.state != BranchState::Unknown);
        if !any_known {
            for b in branches {
                self.edits.extend(b.cond_edits);
                self.visit_branch_block(b.block, b.binding_name.as_deref());
            }
            if let Some(eb) = if_stmt.else_block() {
                self.transform_block(eb, true);
            }
            return semi.is_none();
        }

        let if_raw_start = if_stmt.if_token().start_position().unwrap().bytes();
        let end_tok = if_stmt.end_token();
        let end_raw_start = end_tok.start_position().unwrap().bytes();
        let stmt_raw_end = semi.unwrap_or(end_tok).end_position().unwrap().bytes();

        let first_unknown = branches.iter().position(|b| b.state == BranchState::Unknown);
        let first_true = branches.iter().position(|b| b.state == BranchState::AlwaysTrue);

        // Case 1 & 2: unconditional outcome (first AlwaysTrue precedes any Unknown, or all false)
        let unconditional_win: Option<usize> = match (first_true, first_unknown) {
            (Some(t), None) => Some(t),
            (Some(t), Some(u)) if t < u => Some(t),
            (None, None) => None, // all AlwaysFalse -> else wins (if present) or whole stmt dead
            _ => None,
        };

        let all_false = first_unknown.is_none() && first_true.is_none();

        if all_false && if_stmt.else_block().is_none() {
            // Case 1: entire if...end is dead
            self.emit_dead_stmt_removal(if_raw_start, stmt_raw_end);
            return prev_stmt_has_no_semi;
        }

        if all_false || unconditional_win.is_some() {
            // Case 2: exactly one branch (either branches[t] or else_block) unconditionally executes
            let (win_kw_tok, win_header_end_tok, win_block, dead_tail_start_opt) =
                if let Some(t) = unconditional_win {
                    let tail_start = if t + 1 < branches.len() {
                        Some(branches[t + 1].kw_tok.start_position().unwrap().bytes())
                    } else if let Some(et) = if_stmt.else_token() {
                        Some(et.start_position().unwrap().bytes())
                    } else {
                        Some(end_raw_start)
                    };
                    (
                        branches[t].kw_tok,
                        branches[t].then_tok,
                        branches[t].block,
                        tail_start,
                    )
                } else {
                    let et = if_stmt.else_token().unwrap();
                    let eb = if_stmt.else_block().unwrap();
                    (et, et, eb, None)
                };

            let block_is_empty =
                win_block.stmts().next().is_none() && win_block.last_stmt().is_none();
            if block_is_empty {
                self.emit_dead_stmt_removal(if_raw_start, stmt_raw_end);
                return prev_stmt_has_no_semi;
            }

            let needs_do =
                needs_do_wrapper(win_block, remaining_stmts, enclosing_last, prev_stmt_has_no_semi);

            let win_block_start = win_block.start_position().unwrap().bytes();
            let win_block_end = win_block.end_position().unwrap().bytes();
            let header_raw_end = win_header_end_tok.end_position().unwrap().bytes();

            if needs_do {
                // Header -> `do`
                let h_start = extend_start_if_line_prefix_blank(self.code, if_raw_start);
                let kw_line_start =
                    line_start_of(self.code, win_kw_tok.start_position().unwrap().bytes());
                let h_end = header_raw_end;

                let replacement = if self.config.preserve_lines {
                    if kw_line_start > h_start && only_horiz_ws(&self.code[kw_line_start..win_kw_tok.start_position().unwrap().bytes()]) {
                        let prefix_nls = newlines_only(&self.code[h_start..kw_line_start]);
                        let indent = leading_horiz_ws(self.code, kw_line_start);
                        let after_nls = newlines_only(&self.code[win_kw_tok.start_position().unwrap().bytes()..h_end]);
                        format!("{prefix_nls}{indent}do{after_nls}")
                    } else {
                        let indent = if h_start < if_raw_start {
                            &self.code[h_start..if_raw_start]
                        } else {
                            ""
                        };
                        let inner_nls = newlines_only(&self.code[if_raw_start..h_end]);
                        format!("{indent}do{inner_nls}")
                    }
                } else {
                    let indent = if h_start < if_raw_start {
                        &self.code[h_start..if_raw_start]
                    } else {
                        ""
                    };
                    format!("{indent}do")
                };

                self.edits.push(SpanEdit {
                    start: h_start,
                    end: h_end,
                    replacement,
                });

                self.transform_block(win_block, true);

                // Tail before `end` (keep `end`)
                if let Some(dead_tail_raw_start) = dead_tail_start_opt
                    && dead_tail_raw_start < end_raw_start
                {
                    let t_start =
                        extend_start_if_line_prefix_blank(self.code, dead_tail_raw_start);
                    let tail_slice = &self.code[t_start..end_raw_start];
                    let replacement = if self.config.preserve_lines {
                        if let Some(last_nl) = tail_slice.rfind('\n') {
                            let nls = newlines_only(&tail_slice[..=last_nl]);
                            let end_indent = &tail_slice[last_nl + 1..];
                            format!("{nls}{end_indent}")
                        } else {
                            String::new()
                        }
                    } else if let Some(last_nl) = tail_slice.rfind('\n') {
                        let end_indent = &tail_slice[last_nl + 1..];
                        let needs_nl = t_start != line_start_of(self.code, dead_tail_raw_start);
                        if needs_nl {
                            format!("\n{end_indent}")
                        } else {
                            end_indent.to_string()
                        }
                    } else {
                        String::new()
                    };
                    self.edits.push(SpanEdit {
                        start: t_start,
                        end: end_raw_start,
                        replacement,
                    });
                }
            } else {
                // Unwrap block directly without `do ... end`
                let h_start = extend_start_if_line_prefix_blank(self.code, if_raw_start);
                let win_ls = line_start_of(self.code, win_block_start);
                let (header_end, header_rep) = if win_ls > header_raw_end {
                    let e = if self.config.preserve_lines {
                        extend_end_horiz_if_eol(self.code, header_raw_end).min(win_block_start)
                    } else if h_start == line_start_of(self.code, if_raw_start) {
                        extend_end_through_line_if_blank(self.code, header_raw_end)
                            .min(win_block_start)
                    } else {
                        header_raw_end
                    };
                    let rep = blank_span_text(self.code, h_start, e, self.config.preserve_lines);
                    (e, rep)
                } else {
                    let mut e = header_raw_end;
                    while e < win_block_start
                        && (self.code.as_bytes()[e] == b' ' || self.code.as_bytes()[e] == b'\t')
                    {
                        e += 1;
                    }
                    let rep = if win_ls <= h_start {
                        self.code[h_start..if_raw_start].to_string()
                    } else {
                        let kw_start = win_kw_tok.start_position().unwrap().bytes();
                        let prefix_nls = if self.config.preserve_lines {
                            newlines_only(&self.code[h_start..win_ls])
                        } else {
                            String::new()
                        };
                        let indent = if only_horiz_ws(&self.code[win_ls..kw_start]) {
                            leading_horiz_ws(self.code, win_ls)
                        } else {
                            ""
                        };
                        format!("{prefix_nls}{indent}")
                    };
                    (e, rep)
                };

                self.edits.push(SpanEdit {
                    start: h_start,
                    end: header_end,
                    replacement: header_rep,
                });

                self.transform_block(win_block, false);

                let dead_tail_raw_start = dead_tail_start_opt.unwrap_or(end_raw_start);
                let mut t_start = extend_start_if_line_prefix_blank(self.code, dead_tail_raw_start);
                // Trim horizontal space immediately after win_block when `end` is on the same line
                if t_start == dead_tail_raw_start {
                    while t_start > win_block_end
                        && matches!(self.code.as_bytes()[t_start - 1], b' ' | b'\t')
                    {
                        t_start -= 1;
                    }
                }

                if self.config.preserve_lines {
                    self.edits.push(SpanEdit {
                        start: t_start,
                        end: stmt_raw_end,
                        replacement: newlines_only(&self.code[t_start..stmt_raw_end]),
                    });
                } else if t_start == line_start_of(self.code, dead_tail_raw_start) {
                    let t_end = extend_end_through_line_if_blank(self.code, stmt_raw_end);
                    self.edits.push(SpanEdit {
                        start: t_start,
                        end: t_end,
                        replacement: String::new(),
                    });
                } else {
                    self.edits.push(SpanEdit {
                        start: t_start,
                        end: stmt_raw_end,
                        replacement: String::new(),
                    });
                }
            }
            return if needs_do {
                semi.is_none()
            } else {
                block_last_stmt_has_no_semi(win_block)
            };
        }

        // Case 3: partial reduction — at least one Unknown branch exists (`first_unknown` is Some)
        let first_u = first_unknown.unwrap();
        if first_u > 0 {
            let start = extend_start_if_line_prefix_blank(self.code, if_raw_start);
            let target_kw_start = branches[first_u].kw_tok.start_position().unwrap().bytes();
            let target_line_start = line_start_of(self.code, target_kw_start);
            let blank_end = if target_line_start > start
                && only_horiz_ws(&self.code[target_line_start..target_kw_start])
            {
                target_line_start
            } else {
                target_kw_start
            };
            self.edits.push(SpanEdit {
                start,
                end: blank_end,
                replacement: blank_span_text(self.code, start, blank_end, self.config.preserve_lines),
            });

            let kw_end = branches[first_u].kw_tok.end_position().unwrap().bytes();
            self.edits.push(SpanEdit {
                start: target_kw_start,
                end: kw_end,
                replacement: "if".to_string(),
            });
        }

        let mut i = first_u;
        while i < branches.len() {
            match branches[i].state {
                BranchState::Unknown => {
                    let cond_edits = std::mem::take(&mut branches[i].cond_edits);
                    self.edits.extend(cond_edits);
                    let binding_name = branches[i].binding_name.clone();
                    let block = branches[i].block;
                    self.visit_branch_block(block, binding_name.as_deref());
                    i += 1;
                }
                BranchState::AlwaysFalse => {
                    let kw_start = branches[i].kw_tok.start_position().unwrap().bytes();
                    let del_start = extend_start_if_line_prefix_blank(self.code, kw_start);
                    let next_boundary_start = if i + 1 < branches.len() {
                        branches[i + 1].kw_tok.start_position().unwrap().bytes()
                    } else if let Some(et) = if_stmt.else_token() {
                        et.start_position().unwrap().bytes()
                    } else {
                        end_raw_start
                    };
                    let next_ls = line_start_of(self.code, next_boundary_start);
                    let del_end = if next_ls > del_start
                        && only_horiz_ws(&self.code[next_ls..next_boundary_start])
                    {
                        next_ls
                    } else {
                        next_boundary_start
                    };
                    self.edits.push(SpanEdit {
                        start: del_start,
                        end: del_end,
                        replacement: blank_span_text(
                            self.code,
                            del_start,
                            del_end,
                            self.config.preserve_lines,
                        ),
                    });
                    i += 1;
                }
                BranchState::AlwaysTrue => {
                    debug_assert!(!branches[i].has_binding);
                    let kw_start = branches[i].kw_tok.start_position().unwrap().bytes();
                    let then_end = branches[i].then_tok.end_position().unwrap().bytes();
                    let header_nls = if self.config.preserve_lines {
                        newlines_only(&self.code[kw_start..then_end])
                    } else {
                        String::new()
                    };
                    self.edits.push(SpanEdit {
                        start: kw_start,
                        end: then_end,
                        replacement: format!("else{header_nls}"),
                    });
                    let binding_name = branches[i].binding_name.clone();
                    let block = branches[i].block;
                    self.visit_branch_block(block, binding_name.as_deref());

                    let dead_start_opt = if i + 1 < branches.len() {
                        Some(branches[i + 1].kw_tok.start_position().unwrap().bytes())
                    } else {
                        if_stmt
                            .else_token()
                            .map(|et| et.start_position().unwrap().bytes())
                    };
                    if let Some(dead_kw_start) = dead_start_opt {
                        let del_start = extend_start_if_line_prefix_blank(self.code, dead_kw_start);
                        let end_ls = line_start_of(self.code, end_raw_start);
                        let del_end =
                            if end_ls > del_start && only_horiz_ws(&self.code[end_ls..end_raw_start])
                            {
                                end_ls
                            } else {
                                end_raw_start
                            };
                        self.edits.push(SpanEdit {
                            start: del_start,
                            end: del_end,
                            replacement: blank_span_text(
                                self.code,
                                del_start,
                                del_end,
                                self.config.preserve_lines,
                            ),
                        });
                    }
                    return semi.is_none();
                }
            }
        }

        if let Some(eb) = if_stmt.else_block() {
            self.transform_block(eb, true);
        }
        semi.is_none()
    }

    fn visit_branch_block(&mut self, block: &Block, binding_name: Option<&str>) {
        self.push_scope();
        if let Some(name) = binding_name {
            self.bind_local(name.to_string(), None);
        }
        self.transform_block(block, false);
        self.pop_scope();
    }

    fn eval_expr(&mut self, expr: &Expression, ctx: ExprContext) -> EvalResult {
        if let Some((var_name, start, end)) = self.match_env_var(expr) {
            if let Some(val) = self.resolved.resolve(&var_name) {
                let replacement = self.format_literal_at_span(&val, start, end, ctx, "");
                self.edits.push(SpanEdit {
                    start,
                    end,
                    replacement,
                });
                return EvalResult {
                    value: Some(val),
                    has_env: true,
                };
            }
            return EvalResult::unknown();
        }

        match expr {
            Expression::Var(Var::Name(tok)) => {
                if let Some(name) = identifier_name(tok)
                    && let Some(val) = self.lookup_local(name)
                {
                    return EvalResult {
                        value: Some(val),
                        has_env: true,
                    };
                }
                EvalResult::unknown()
            }
            Expression::Var(Var::Expression(ve)) => {
                if let Prefix::Expression(inner) = ve.prefix() {
                    self.eval_expr(inner, ExprContext::default());
                }
                for suffix in ve.suffixes() {
                    self.transform_suffix(suffix);
                }
                EvalResult::unknown()
            }
            Expression::Symbol(tok) => {
                let val = match tok.token().token_type() {
                    TokenType::Symbol {
                        symbol: Symbol::True,
                    } => Some(StaticValue::Bool(true)),
                    TokenType::Symbol {
                        symbol: Symbol::False,
                    } => Some(StaticValue::Bool(false)),
                    TokenType::Symbol {
                        symbol: Symbol::Nil,
                    } => Some(StaticValue::Nil),
                    _ => None,
                };
                EvalResult {
                    value: val,
                    has_env: false,
                }
            }
            Expression::Number(tok) => {
                let val = if let TokenType::Number { text } = tok.token().token_type() {
                    parse_luau_unsigned_number(text.as_str()).map(StaticValue::Number)
                } else {
                    None
                };
                EvalResult {
                    value: val,
                    has_env: false,
                }
            }
            Expression::String(tok) => EvalResult {
                value: extract_string_literal(tok).map(StaticValue::String),
                has_env: false,
            },
            Expression::Parentheses { expression, .. } => {
                self.eval_expr(expression, ExprContext::default())
            }
            Expression::TypeAssertion { expression, .. } => {
                self.eval_expr(
                    expression,
                    ExprContext {
                        needs_parens_for_non_atomic: true,
                        is_power_lhs: false,
                    },
                )
            }
            Expression::UnaryOperator { unop, expression } => {
                let inner = self.eval_expr(
                    expression,
                    ExprContext {
                        needs_parens_for_non_atomic: true,
                        is_power_lhs: false,
                    },
                );
                let val = inner.value.as_ref().and_then(|v| match (unop, v) {
                    (UnOp::Not(_), v) => Some(StaticValue::Bool(!v.is_truthy())),
                    (UnOp::Minus(_), StaticValue::Number(n)) => Some(StaticValue::Number(-n)),
                    (UnOp::Hash(_), StaticValue::String(s)) => {
                        Some(StaticValue::Number(s.len() as f64))
                    }
                    _ => None,
                });
                EvalResult {
                    value: val,
                    has_env: inner.has_env,
                }
            }
            Expression::BinaryOperator { lhs, binop, rhs } => {
                self.eval_binary_op(lhs, binop, rhs, expr, ctx)
            }
            Expression::IfExpression(if_expr) => self.eval_if_expr(if_expr, ctx),
            Expression::Function(func) => {
                self.transform_function_body(func.body(), false);
                EvalResult::unknown()
            }
            Expression::FunctionCall(fc) => {
                self.transform_function_call(fc);
                EvalResult::unknown()
            }
            Expression::TableConstructor(tc) => {
                self.transform_table_constructor(tc);
                EvalResult::unknown()
            }
            Expression::InterpolatedString(interp) => {
                for seg in interp.segments() {
                    self.eval_expr(&seg.expression, ExprContext::default());
                }
                EvalResult::unknown()
            }
            _ => EvalResult::unknown(),
        }
    }

    fn eval_binary_op(
        &mut self,
        lhs: &Expression,
        binop: &BinOp,
        rhs: &Expression,
        whole_expr: &Expression,
        ctx: ExprContext,
    ) -> EvalResult {
        let cp = self.edits.len();
        let lhs_ctx = ExprContext {
            needs_parens_for_non_atomic: true,
            is_power_lhs: matches!(binop, BinOp::Caret(_)),
        };
        let lhs_eval = self.eval_expr(lhs, lhs_ctx);

        if self.config.dead_code_elimination
            && lhs_eval.has_env
            && let Some(ref lv) = lhs_eval.value
        {
            let short_circuit = matches!(binop, BinOp::And(_)) && !lv.is_truthy()
                || matches!(binop, BinOp::Or(_)) && lv.is_truthy();
            if short_circuit {
                self.edits.truncate(cp);
                let start = whole_expr.start_position().unwrap().bytes();
                let end = whole_expr.end_position().unwrap().bytes();
                let extra_nls = if self.config.preserve_lines {
                    newlines_only(&self.code[start..end])
                } else {
                    String::new()
                };
                let replacement =
                    self.format_literal_at_span(lv, start, end, ctx, &extra_nls);
                self.edits.push(SpanEdit {
                    start,
                    end,
                    replacement,
                });
                return EvalResult {
                    value: Some(lv.clone()),
                    has_env: true,
                };
            }
        }

        let rhs_eval = self.eval_expr(
            rhs,
            ExprContext {
                needs_parens_for_non_atomic: true,
                is_power_lhs: false,
            },
        );
        let has_env = lhs_eval.has_env || rhs_eval.has_env;

        let val = match binop {
            BinOp::And(_) => match (&lhs_eval.value, &rhs_eval.value) {
                (Some(lv), _) if !lv.is_truthy() => Some(lv.clone()),
                (Some(_), Some(rv)) => Some(rv.clone()),
                _ => None,
            },
            BinOp::Or(_) => match (&lhs_eval.value, &rhs_eval.value) {
                (Some(lv), _) if lv.is_truthy() => Some(lv.clone()),
                (Some(_), Some(rv)) => Some(rv.clone()),
                _ => None,
            },
            _ => match (&lhs_eval.value, &rhs_eval.value) {
                (Some(lv), Some(rv)) => eval_binary_values(binop, lv, rv),
                _ => None,
            },
        };

        EvalResult { value: val, has_env }
    }

    fn eval_if_expr(&mut self, if_expr: &IfExpression, ctx: ExprContext) -> EvalResult {
        struct ExprBranch<'b> {
            kw_tok: &'b TokenReference,
            then_tok: &'b TokenReference,
            has_binding: bool,
            expr: &'b Expression,
            state: BranchState,
            cond_edits: Vec<SpanEdit>,
        }

        let mut branches = Vec::new();
        {
            let cp = self.edits.len();
            let eval = self.eval_expr(if_expr.condition(), ExprContext::default());
            let cond_edits: Vec<SpanEdit> = self.edits.drain(cp..).collect();
            let has_binding = if_expr.binding().is_some();
            let state = classify_branch(self.config.dead_code_elimination, has_binding, &eval);
            branches.push(ExprBranch {
                kw_tok: if_expr.if_token(),
                then_tok: if_expr.then_token(),
                has_binding,
                expr: if_expr.if_expression(),
                state,
                cond_edits,
            });
        }

        if let Some(else_ifs) = if_expr.else_if_expressions() {
            for eif in else_ifs {
                let cp = self.edits.len();
                let eval = self.eval_expr(eif.condition(), ExprContext::default());
                let cond_edits: Vec<SpanEdit> = self.edits.drain(cp..).collect();
                let has_binding = eif.binding().is_some();
                let state = classify_branch(self.config.dead_code_elimination, has_binding, &eval);
                branches.push(ExprBranch {
                    kw_tok: eif.else_if_token(),
                    then_tok: eif.then_token(),
                    has_binding,
                    expr: eif.expression(),
                    state,
                    cond_edits,
                });
            }
        }

        let any_known = branches.iter().any(|b| b.state != BranchState::Unknown);
        if !any_known {
            for b in branches {
                self.edits.extend(b.cond_edits);
                self.eval_expr(b.expr, ExprContext::default());
            }
            self.eval_expr(if_expr.else_expression(), ExprContext::default());
            return EvalResult::unknown();
        }

        let first_unknown = branches.iter().position(|b| b.state == BranchState::Unknown);
        let first_true = branches.iter().position(|b| b.state == BranchState::AlwaysTrue);

        let unconditional_win: Option<usize> = match (first_true, first_unknown) {
            (Some(t), None) => Some(t),
            (Some(t), Some(u)) if t < u => Some(t),
            _ => None,
        };
        let all_false = first_unknown.is_none() && first_true.is_none();

        let if_start = if_expr.if_token().start_position().unwrap().bytes();
        let else_tok = if_expr.else_token();
        let else_expr = if_expr.else_expression();
        let else_expr_end = else_expr.end_position().unwrap().bytes();

        if all_false || unconditional_win.is_some() {
            let (win_expr, has_dead_tail) = if let Some(t) = unconditional_win {
                (branches[t].expr, true)
            } else {
                (else_expr, false)
            };

            let win_start = win_expr.start_position().unwrap().bytes();
            let win_end = win_expr.end_position().unwrap().bytes();
            let wrap_parens = ctx.needs_parens_for_non_atomic && !is_atomic_expr(win_expr);

            let win_line_start = line_start_of(self.code, win_start);
            let header_rep = if self.config.preserve_lines {
                if win_line_start > if_start {
                    let prefix_nls = newlines_only(&self.code[if_start..win_line_start]);
                    let indent = leading_horiz_ws(self.code, win_line_start);
                    format!("{prefix_nls}{indent}")
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            self.edits.push(SpanEdit {
                start: if_start,
                end: win_start,
                replacement: header_rep,
            });

            if wrap_parens {
                self.edits.push(SpanEdit {
                    start: win_start,
                    end: win_start,
                    replacement: "(".to_string(),
                });
            }

            let inner_ctx = ExprContext {
                needs_parens_for_non_atomic: if wrap_parens {
                    false
                } else {
                    ctx.needs_parens_for_non_atomic
                },
                is_power_lhs: if wrap_parens { false } else { ctx.is_power_lhs },
            };
            let mut res = self.eval_expr(win_expr, inner_ctx);
            res.has_env = true;

            if wrap_parens {
                self.edits.push(SpanEdit {
                    start: win_end,
                    end: win_end,
                    replacement: ")".to_string(),
                });
            }

            if has_dead_tail && win_end < else_expr_end {
                let tail_rep = blank_span_text(
                    self.code,
                    win_end,
                    else_expr_end,
                    self.config.preserve_lines,
                );
                self.edits.push(SpanEdit {
                    start: win_end,
                    end: else_expr_end,
                    replacement: tail_rep,
                });
            }

            return res;
        }

        // Partial reduction for IfExpression
        let first_u = first_unknown.unwrap();
        if first_u > 0 {
            let target_kw_start = branches[first_u].kw_tok.start_position().unwrap().bytes();
            let target_ls = line_start_of(self.code, target_kw_start);
            let blank_end = if target_ls > if_start
                && only_horiz_ws(&self.code[target_ls..target_kw_start])
            {
                target_ls
            } else {
                target_kw_start
            };
            self.edits.push(SpanEdit {
                start: if_start,
                end: blank_end,
                replacement: blank_span_text(
                    self.code,
                    if_start,
                    blank_end,
                    self.config.preserve_lines,
                ),
            });

            let kw_end = branches[first_u].kw_tok.end_position().unwrap().bytes();
            self.edits.push(SpanEdit {
                start: target_kw_start,
                end: kw_end,
                replacement: "if".to_string(),
            });
        }

        let mut i = first_u;
        while i < branches.len() {
            match branches[i].state {
                BranchState::Unknown => {
                    let cond_edits = std::mem::take(&mut branches[i].cond_edits);
                    self.edits.extend(cond_edits);
                    self.eval_expr(branches[i].expr, ExprContext::default());
                    i += 1;
                }
                BranchState::AlwaysFalse => {
                    let kw_start = branches[i].kw_tok.start_position().unwrap().bytes();
                    let del_start = extend_start_if_line_prefix_blank(self.code, kw_start);
                    let next_boundary_start = if i + 1 < branches.len() {
                        branches[i + 1].kw_tok.start_position().unwrap().bytes()
                    } else {
                        else_tok.start_position().unwrap().bytes()
                    };
                    let next_ls = line_start_of(self.code, next_boundary_start);
                    let del_end = if next_ls > del_start
                        && only_horiz_ws(&self.code[next_ls..next_boundary_start])
                    {
                        next_ls
                    } else {
                        next_boundary_start
                    };
                    self.edits.push(SpanEdit {
                        start: del_start,
                        end: del_end,
                        replacement: blank_span_text(
                            self.code,
                            del_start,
                            del_end,
                            self.config.preserve_lines,
                        ),
                    });
                    i += 1;
                }
                BranchState::AlwaysTrue => {
                    debug_assert!(!branches[i].has_binding);
                    let kw_start = branches[i].kw_tok.start_position().unwrap().bytes();
                    let then_end = branches[i].then_tok.end_position().unwrap().bytes();
                    let header_nls = if self.config.preserve_lines {
                        newlines_only(&self.code[kw_start..then_end])
                    } else {
                        String::new()
                    };
                    self.edits.push(SpanEdit {
                        start: kw_start,
                        end: then_end,
                        replacement: format!("else{header_nls}"),
                    });
                    self.eval_expr(branches[i].expr, ExprContext::default());
                    let expr_end = branches[i].expr.end_position().unwrap().bytes();
                    if expr_end < else_expr_end {
                        self.edits.push(SpanEdit {
                            start: expr_end,
                            end: else_expr_end,
                            replacement: blank_span_text(
                                self.code,
                                expr_end,
                                else_expr_end,
                                self.config.preserve_lines,
                            ),
                        });
                    }
                    return EvalResult::unknown();
                }
            }
        }

        self.eval_expr(else_expr, ExprContext::default());
        EvalResult::unknown()
    }
}

fn classify_branch(
    dead_code_elimination: bool,
    has_binding: bool,
    eval: &EvalResult,
) -> BranchState {
    if !dead_code_elimination || has_binding || !eval.has_env {
        return BranchState::Unknown;
    }
    match &eval.value {
        Some(val) if val.is_truthy() => BranchState::AlwaysTrue,
        Some(_) => BranchState::AlwaysFalse,
        None => BranchState::Unknown,
    }
}

fn eval_binary_values(binop: &BinOp, lv: &StaticValue, rv: &StaticValue) -> Option<StaticValue> {
    match binop {
        BinOp::TwoEqual(_) => Some(StaticValue::Bool(lv.luau_eq(rv))),
        BinOp::TildeEqual(_) => Some(StaticValue::Bool(!lv.luau_eq(rv))),
        BinOp::LessThan(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b)) => Some(StaticValue::Bool(a < b)),
            (StaticValue::String(a), StaticValue::String(b)) => Some(StaticValue::Bool(a < b)),
            _ => None,
        },
        BinOp::LessThanEqual(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b)) => Some(StaticValue::Bool(a <= b)),
            (StaticValue::String(a), StaticValue::String(b)) => Some(StaticValue::Bool(a <= b)),
            _ => None,
        },
        BinOp::GreaterThan(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b)) => Some(StaticValue::Bool(a > b)),
            (StaticValue::String(a), StaticValue::String(b)) => Some(StaticValue::Bool(a > b)),
            _ => None,
        },
        BinOp::GreaterThanEqual(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b)) => Some(StaticValue::Bool(a >= b)),
            (StaticValue::String(a), StaticValue::String(b)) => Some(StaticValue::Bool(a >= b)),
            _ => None,
        },
        BinOp::Plus(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b)) if (a + b).is_finite() => {
                Some(StaticValue::Number(a + b))
            }
            _ => None,
        },
        BinOp::Minus(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b)) if (a - b).is_finite() => {
                Some(StaticValue::Number(a - b))
            }
            _ => None,
        },
        BinOp::Star(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b)) if (a * b).is_finite() => {
                Some(StaticValue::Number(a * b))
            }
            _ => None,
        },
        BinOp::Slash(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b))
                if *b != 0.0 && (a / b).is_finite() =>
            {
                Some(StaticValue::Number(a / b))
            }
            _ => None,
        },
        BinOp::DoubleSlash(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b)) if *b != 0.0 => {
                let res = (a / b).floor();
                if res.is_finite() {
                    Some(StaticValue::Number(res))
                } else {
                    None
                }
            }
            _ => None,
        },
        BinOp::Percent(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b)) if *b != 0.0 => {
                let res = a - (a / b).floor() * b;
                if res.is_finite() {
                    Some(StaticValue::Number(res))
                } else {
                    None
                }
            }
            _ => None,
        },
        BinOp::Caret(_) => match (lv, rv) {
            (StaticValue::Number(a), StaticValue::Number(b)) if a.powf(*b).is_finite() => {
                Some(StaticValue::Number(a.powf(*b)))
            }
            _ => None,
        },
        BinOp::TwoDots(_) => match (lv, rv) {
            (StaticValue::String(a), StaticValue::String(b)) => {
                Some(StaticValue::String(format!("{a}{b}")))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Transforms Luau source code by injecting environment variables and optionally
/// performing static dead-code elimination while preserving line numbers if configured.
pub fn transform_env(
    code: &str,
    config: &EnvConfig,
    resolved_env: &ResolvedEnv<'_>,
) -> Result<(String, bool), Vec<full_moon::Error>> {
    if !config.enabled {
        return Ok((code.to_string(), false));
    }

    let ast = full_moon::parse(code)?;
    let mut transformer = EnvTransformer::new(code, config, resolved_env);
    transformer.transform_ast(&ast);

    if transformer.edits.is_empty() {
        return Ok((code.to_string(), false));
    }

    let result = apply_edits(code, transformer.edits);
    let modified = result != code;
    Ok((result, modified))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn run_env(code: &str, pairs: &[(&str, &str)], config: EnvConfig) -> (String, bool) {
        let vars = make_vars(pairs);
        let resolved = ResolvedEnv::new(&config, &vars).expect("env resolution failed");
        let res = transform_env(code, &config, &resolved).expect("transform_env failed");
        full_moon::parse(&res.0).unwrap_or_else(|e| {
            panic!(
                "Transformed code failed to parse!\nErrors: {:?}\nOutput:\n{}",
                e, res.0
            )
        });
        res
    }

    #[test]
    fn test_auto_casting_and_string_escaping() {
        let input = r#"
local dev = _G.DEV
local prod = _G.PROD
local nothing = _G.NONE
local port = _G.PORT
local neg = _G.NEG
local zero = _G.ZERO
local pi = _G.PI
local id = _G.PADDED_ID
local nan_str = _G.NOT_NUM
local msg = _G.MSG
"#;
        let (out, modified) = run_env(
            input,
            &[
                ("DEV", "true"),
                ("PROD", "false"),
                ("NONE", "nil"),
                ("PORT", "8080"),
                ("NEG", "-5"),
                ("ZERO", "0"),
                ("PI", "3.14"),
                ("PADDED_ID", "007"),
                ("NOT_NUM", "NaN"),
                ("MSG", "hello \"world\"\nline2\\end"),
            ],
            EnvConfig::default(),
        );
        assert!(modified);
        assert!(out.contains("local dev = true"));
        assert!(out.contains("local prod = false"));
        assert!(out.contains("local nothing = nil"));
        assert!(out.contains("local port = 8080"));
        assert!(out.contains("local neg = -5"));
        assert!(out.contains("local zero = 0"));
        assert!(out.contains("local pi = 3.14"));
        assert!(out.contains(r#"local id = "007""#));
        assert!(out.contains(r#"local nan_str = "NaN""#));
        assert!(out.contains(r#"local msg = "hello \"world\"\nline2\\end""#));
        assert_eq!(out.lines().count(), input.lines().count());
    }

    #[test]
    fn test_cast_false_and_per_var_types() {
        let mut types = HashMap::new();
        types.insert("PORT".to_string(), EnvValueType::Number);
        types.insert("DEV".to_string(), EnvValueType::Bool);
        types.insert("CODE".to_string(), EnvValueType::String);

        let cfg = EnvConfig {
            cast: false,
            types,
            ..EnvConfig::default()
        };

        let input = r#"
local rawFlag = _G.RAW_BOOL
local rawNum = _G.RAW_NUM
local port = _G.PORT
local dev = _G.DEV
local code = _G.CODE
"#;
        let (out, _) = run_env(
            input,
            &[
                ("RAW_BOOL", "true"),
                ("RAW_NUM", "42"),
                ("PORT", "3000"),
                ("DEV", "1"),
                ("CODE", "123"),
            ],
            cfg,
        );
        assert!(out.contains(r#"local rawFlag = "true""#));
        assert!(out.contains(r#"local rawNum = "42""#));
        assert!(out.contains("local port = 3000"));
        assert!(out.contains("local dev = true"));
        assert!(out.contains(r#"local code = "123""#));
    }

    #[test]
    fn test_invalid_explicit_cast_returns_error() {
        let mut types = HashMap::new();
        types.insert("PORT".to_string(), EnvValueType::Number);
        let cfg = EnvConfig {
            types,
            ..EnvConfig::default()
        };
        let vars = make_vars(&[("PORT", "not_a_number")]);
        let err = ResolvedEnv::new(&cfg, &vars).err().expect("expected error");
        assert!(err.contains("PORT"));
    }

    #[test]
    fn test_defaults_and_unknown_untouched() {
        let mut types = HashMap::new();
        types.insert("TYPED_UNSET".to_string(), EnvValueType::Bool);
        let mut defaults = HashMap::new();
        defaults.insert("MODE".to_string(), toml::Value::String("local".to_string()));

        let cfg = EnvConfig {
            types,
            defaults,
            ..EnvConfig::default()
        };
        let input = r#"
local mode = _G.MODE
local typed = _G.TYPED_UNSET
local other = _G.SomeRuntimeGlobal
"#;
        let (out, _) = run_env(input, &[], cfg);
        assert!(out.contains(r#"local mode = "local""#));
        assert!(out.contains("local typed = nil"));
        assert!(out.contains("local other = _G.SomeRuntimeGlobal"));
    }

    #[test]
    fn test_bracket_syntax_and_token_hazards() {
        let input = r#"
_G.DEV = true
local t = {}
t[_G.KEY] = 1
local a = 10-_G.NEG
local b = _G.NEG^2
local c = _G.PORT.."!"
if _G["DEV"]then print("ok") end
local untouched = _G.UNSET_GLOBAL
"#;
        let (out, _) = run_env(
            input,
            &[("DEV", "true"), ("KEY", "slot"), ("NEG", "-5"), ("PORT", "8080")],
            EnvConfig::default(),
        );
        assert!(out.contains("_G.DEV = true")); // LHS not replaced!
        assert!(out.contains(r#"t["slot"] = 1"#));
        assert!(out.contains("local a = 10-(-5)"));
        assert!(out.contains("local b = (-5)^2"));
        assert!(out.contains(r#"local c = 8080 .."!""#));
        assert!(out.contains(r#"print("ok")"#));
        assert!(out.contains("local untouched = _G.UNSET_GLOBAL"));
    }

    #[test]
    fn test_if_stmt_dce_preserve_lines_true_and_false() {
        let input = "\
local top = 1
if _G.DEV then
    local devOnly = require(\"@pkg/Dev\")
    print(devOnly)
else
    local prodOnly = require(\"@pkg/Prod\")
    print(prodOnly)
end
local after = \"marker_line_9\"
";
        // DEV = true, preserve_lines = true
        let (out_true, _) = run_env(input, &[("DEV", "true")], EnvConfig::default());
        assert!(out_true.contains("devOnly"));
        assert!(!out_true.contains("prodOnly"));
        assert!(!out_true.contains("if "));
        assert!(!out_true.contains("else"));
        assert!(!out_true.contains("end"));
        let line_after_true = out_true
            .lines()
            .position(|l| l.contains("marker_line_9"))
            .unwrap()
            + 1;
        assert_eq!(line_after_true, 9);
        let line_dev_true = out_true
            .lines()
            .position(|l| l.contains("local devOnly"))
            .unwrap()
            + 1;
        assert_eq!(line_dev_true, 3);

        // DEV = false, preserve_lines = true
        let (out_false, _) = run_env(input, &[("DEV", "false")], EnvConfig::default());
        assert!(!out_false.contains("devOnly"));
        assert!(out_false.contains("prodOnly"));
        let line_after_false = out_false
            .lines()
            .position(|l| l.contains("marker_line_9"))
            .unwrap()
            + 1;
        assert_eq!(line_after_false, 9);
        let line_prod_false = out_false
            .lines()
            .position(|l| l.contains("local prodOnly"))
            .unwrap()
            + 1;
        assert_eq!(line_prod_false, 6);

        // DEV = false, preserve_lines = false
        let cfg_compact = EnvConfig {
            preserve_lines: false,
            ..EnvConfig::default()
        };
        let (out_compact, _) = run_env(input, &[("DEV", "false")], cfg_compact);
        assert_eq!(
            out_compact,
            "local top = 1\n    local prodOnly = require(\"@pkg/Prod\")\n    print(prodOnly)\nlocal after = \"marker_line_9\"\n"
        );
    }

    #[test]
    fn test_if_stmt_no_else_and_elseif_chains() {
        let input = "\
local a = 1
if _G.DEV then
    debugPrint()
end
if _G.A then
    branchA()
elseif runtimeCheck() then
    branchRuntime()
elseif _G.B then
    branchB()
elseif _G.C then
    branchC()
else
    branchElse()
end
local z = \"line_16\"
";
        let (out, _) = run_env(
            input,
            &[("DEV", "false"), ("A", "false"), ("B", "true"), ("C", "false")],
            EnvConfig::default(),
        );
        assert!(!out.contains("debugPrint"));
        assert!(!out.contains("branchA"));
        assert!(out.contains("if runtimeCheck() then"));
        assert!(out.contains("branchRuntime()"));
        assert!(out.contains("else\n    branchB()"));
        assert!(!out.contains("branchC"));
        assert!(!out.contains("branchElse"));
        let line_z = out.lines().position(|l| l.contains("line_16")).unwrap() + 1;
        assert_eq!(line_z, 16);
    }

    #[test]
    fn test_scoping_do_wrapper_and_early_return() {
        let input = "\
local x = \"outer\"
if _G.DEV then
    local x = \"inner\"
    print(x)
end
print(x)
if _G.EARLY then
    return 1
end
print(\"after_early\")
";
        let (out, _) = run_env(
            input,
            &[("DEV", "true"), ("EARLY", "true")],
            EnvConfig::default(),
        );
        // Both blocks must be wrapped in `do ... end` to preserve `x` shadowing and allow code after `return 1`
        assert!(out.contains("do\n    local x = \"inner\""));
        assert!(out.contains("do\n    return 1\nend"));
        let line_after = out.lines().position(|l| l.contains("after_early")).unwrap() + 1;
        assert_eq!(line_after, 10);
    }

    #[test]
    fn test_local_propagation_and_mutation_shadowing_safety() {
        let input = "\
local isDev = _G.DEV
local mutFlag = _G.DEV
mutFlag = false
local function helper(isDev)
    if isDev then
        return 1
    end
    return 2
end
if isDev then
    print(\"is_dev_branch\")
else
    print(\"not_dev_branch\")
end
if mutFlag then
    print(\"mut_true\")
else
    print(\"mut_false\")
end
";
        let (out, _) = run_env(input, &[("DEV", "true")], EnvConfig::default());
        // Inside helper, parameter `isDev` shadows outer `isDev`, so `if isDev` is kept!
        assert!(out.contains("if isDev then\n        return 1"));
        // Outer immutable `isDev` propagates into `if isDev`, pruning `else`
        assert!(out.contains("print(\"is_dev_branch\")"));
        assert!(!out.contains("not_dev_branch"));
        // Mutated `mutFlag` is NOT statically folded in `if mutFlag`
        assert!(out.contains("if mutFlag then"));
        assert!(out.contains("mut_true"));
        assert!(out.contains("mut_false"));
    }

    #[test]
    fn test_if_expression_short_circuit_and_while() {
        let input = "\
local a = 2 * if _G.DEV then 1 + 2 else 10
local b = if _G.A
    then \"aaa\"
    else \"bbb\"
local c = _G.DEV and require(\"@pkg/Dev\")
local d = _G.API or \"https://prod.example.com\"
while _G.LOOP do
    print(\"never\")
end
if false then
    print(\"non_env_untouched\")
end
local done = \"line_13\"
";
        let (out, _) = run_env(
            input,
            &[
                ("DEV", "false"),
                ("A", "true"),
                ("API", "https://dev.example.com"),
                ("LOOP", "false"),
            ],
            EnvConfig::default(),
        );
        assert!(out.contains("local a = 2 * 10"));
        assert!(out.contains("\"aaa\""));
        assert!(!out.contains("\"bbb\""));
        assert!(out.contains("local c = false"));
        assert!(!out.contains("@pkg/Dev"));
        assert!(out.contains(r#"local d = "https://dev.example.com""#));
        assert!(!out.contains("never"));
        assert!(out.contains("if false then\n    print(\"non_env_untouched\")\nend"));
        let line_done = out.lines().position(|l| l.contains("line_13")).unwrap() + 1;
        assert_eq!(line_done, 13);

        let input_true = "local a = 2 * if _G.DEV then 1 + 2 else 10";
        let (out_t, _) = run_env(input_true, &[("DEV", "true")], EnvConfig::default());
        assert_eq!(out_t, "local a = 2 * (1 + 2)");
    }

    #[test]
    fn test_dead_code_elimination_disabled() {
        let cfg = EnvConfig {
            dead_code_elimination: false,
            ..EnvConfig::default()
        };
        let input = "\
if _G.DEV then
    print(\"dev\")
else
    print(\"prod\")
end
";
        let (out, modified) = run_env(input, &[("DEV", "false")], cfg);
        assert!(modified);
        assert!(out.contains("if false then"));
        assert!(out.contains("print(\"dev\")"));
        assert!(out.contains("print(\"prod\")"));
    }

    #[test]
    fn test_if_condition_binding_not_eliminated() {
        let input = "\
if local x = _G.DEV then
    print(x)
else
    print(\"fallback\")
end
";
        let (out, modified) = run_env(input, &[("DEV", "true")], EnvConfig::default());
        assert!(modified);
        assert!(out.contains("if local x = true then"));
        assert!(out.contains("print(x)"));
        assert!(out.contains("print(\"fallback\")"));
    }

    #[test]
    fn test_ambiguous_paren_across_dead_if_and_inline_indent() {
        let input = "\
local f = getFn();
if _G.DEAD then
    dead()
end
if _G.LIVE then
    (a or b):run()
end
    if _G.LIVE then local inlineA = 1 end
if _G.DEAD then
    dead2()
else inlineElse() end
";
        // Notice first statement had `;`, wait: test without `;` on local f!
        let input_no_semi = input.replacen("local f = getFn();", "local f = getFn()", 1);
        let (out, _) = run_env(
            &input_no_semi,
            &[("DEAD", "false"), ("LIVE", "true")],
            EnvConfig::default(),
        );
        assert!(out.contains("do\n    (a or b):run()\nend"));
        assert!(out.contains("    local inlineA = 1"));
        assert!(out.contains("\ninlineElse()"));
    }

    #[test]
    fn test_arithmetic_and_string_comparison_folding() {
        let input = "\
local sum = _G.A + _G.B
if sum == 5 and _G.MODE == \"dev\" then
    print(\"matched\")
else
    print(\"missed\")
end
";
        let (out, _) = run_env(
            input,
            &[("A", "2"), ("B", "3"), ("MODE", "dev")],
            EnvConfig::default(),
        );
        assert!(out.contains("local sum = 2 + 3"));
        assert!(out.contains("print(\"matched\")"));
        assert!(!out.contains("missed"));
    }

    #[test]
    fn test_required_env_vars() {
        let cfg = EnvConfig {
            required: vec!["API_URL".to_string(), "SECRET".to_string()],
            ..EnvConfig::default()
        };

        let partial = make_vars(&[("API_URL", "https://example.com")]);
        let err = ResolvedEnv::new(&cfg, &partial)
            .err()
            .expect("expected missing required var error");
        assert!(err.contains("SECRET"));
        assert!(!err.contains("API_URL"));

        let empty = make_vars(&[]);
        let err_both = ResolvedEnv::new(&cfg, &empty)
            .err()
            .expect("expected missing required vars error");
        assert!(err_both.contains("API_URL"));
        assert!(err_both.contains("SECRET"));

        let complete = make_vars(&[("API_URL", "https://example.com"), ("SECRET", "ok")]);
        assert!(ResolvedEnv::new(&cfg, &complete).is_ok());
    }
}
