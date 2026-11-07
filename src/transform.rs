use full_moon::ast::punctuated::{Pair, Punctuated};
use full_moon::ast::{Call, Expression, FunctionArgs, FunctionCall, Prefix, Suffix, VarExpression};
use full_moon::tokenizer::{Token, TokenType};
use full_moon::visitors::VisitorMut;
use full_moon::ShortString;
use std::collections::{HashMap, HashSet};

pub struct RequireAliasReplacer<'a> {
    pub valid_luaurc_aliases: &'a HashSet<String>,
    pub alias_expansions: &'a HashMap<String, String>,
    pub modified: bool,
}

impl<'a> RequireAliasReplacer<'a> {
    pub fn new(
        valid_luaurc_aliases: &'a HashSet<String>,
        alias_expansions: &'a HashMap<String, String>,
    ) -> Self {
        Self {
            valid_luaurc_aliases,
            alias_expansions,
            modified: false,
        }
    }

    fn expand_string_literal(&mut self, token: &Token) -> Option<Token> {
        let (literal, multi_line_depth, quote_type) = match token.token_type() {
            TokenType::StringLiteral {
                literal,
                multi_line_depth,
                quote_type,
            } => (literal, *multi_line_depth, *quote_type),
            _ => return None,
        };

        let lit_str = literal.as_str();
        let alias_part = lit_str.strip_prefix('@')?;

        let (alias_name, subpath) = match alias_part.split_once('/') {
            Some((name, rest)) => (name, Some(rest)),
            None => (alias_part, None),
        };

        // In a Roblox environment, `@game` is default and should be ignored
        if alias_name == "game" {
            return None;
        }

        // Must be defined in the corresponding .luaurc
        if !self.valid_luaurc_aliases.contains(alias_name) {
            return None;
        }

        // Must have an expansion defined in limon.toml
        let replacement = self.alias_expansions.get(alias_name)?;

        let expanded = match subpath {
            Some(sub) => {
                let trimmed_rep = replacement.trim_end_matches('/');
                let trimmed_sub = sub.trim_start_matches('/');
                format!("{}/{}", trimmed_rep, trimmed_sub)
            }
            None => replacement.clone(),
        };

        self.modified = true;

        Some(Token::new(TokenType::StringLiteral {
            literal: ShortString::new(&expanded),
            multi_line_depth,
            quote_type,
        }))
    }

    fn transform_require_suffixes<'s>(
        &mut self,
        prefix: &Prefix,
        suffixes: impl Iterator<Item = &'s Suffix>,
    ) -> Option<Vec<Suffix>> {
        let is_require = match prefix {
            Prefix::Name(name_tok) => name_tok.token().to_string().trim() == "require",
            _ => false,
        };

        if !is_require {
            return None;
        }

        let mut new_suffixes = Vec::new();
        let mut first_call_handled = false;
        let mut changed_any = false;

        for suffix in suffixes {
            #[allow(clippy::collapsible_if)]
            if !first_call_handled {
                if let Suffix::Call(Call::AnonymousCall(args)) = suffix {
                    first_call_handled = true;
                    let (new_args, changed) = match &**args {
                        FunctionArgs::Parentheses {
                            arguments,
                            parentheses,
                        } => {
                            let mut new_arguments = Punctuated::new();
                            let mut is_first_arg = true;
                            let mut arg_changed = false;

                            for pair in arguments.pairs() {
                                let expr = pair.value();
                                let new_expr = if is_first_arg {
                                    is_first_arg = false;
                                    match expr {
                                        Expression::String(tok_ref) => {
                                            if let Some(new_tok) =
                                                self.expand_string_literal(tok_ref.token())
                                            {
                                                arg_changed = true;
                                                Expression::String(tok_ref.with_token(new_tok))
                                            } else {
                                                expr.clone()
                                            }
                                        }
                                        other => other.clone(),
                                    }
                                } else {
                                    expr.clone()
                                };

                                match pair.punctuation() {
                                    Some(punct) => new_arguments
                                        .push(Pair::Punctuated(new_expr, punct.clone())),
                                    None => new_arguments.push(Pair::End(new_expr)),
                                }
                            }

                            (
                                FunctionArgs::Parentheses {
                                    parentheses: parentheses.clone(),
                                    arguments: new_arguments,
                                },
                                arg_changed,
                            )
                        }
                        FunctionArgs::String(tok_ref) => {
                            if let Some(new_tok) = self.expand_string_literal(tok_ref.token()) {
                                (FunctionArgs::String(tok_ref.with_token(new_tok)), true)
                            } else {
                                (FunctionArgs::String(tok_ref.clone()), false)
                            }
                        }
                        other => (other.clone(), false),
                    };

                    if changed {
                        changed_any = true;
                    }
                    new_suffixes.push(Suffix::Call(Call::AnonymousCall(Box::new(new_args))));
                    continue;
                }
            }
            new_suffixes.push(suffix.clone());
        }

        if changed_any {
            Some(new_suffixes)
        } else {
            None
        }
    }
}

impl<'a> VisitorMut for RequireAliasReplacer<'a> {
    fn visit_function_call_end(&mut self, call: FunctionCall) -> FunctionCall {
        if let Some(new_suffixes) = self.transform_require_suffixes(call.prefix(), call.suffixes()) {
            call.with_suffixes(new_suffixes)
        } else {
            call
        }
    }

    fn visit_var_expression_end(&mut self, var_expr: VarExpression) -> VarExpression {
        if let Some(new_suffixes) =
            self.transform_require_suffixes(var_expr.prefix(), var_expr.suffixes())
        {
            var_expr.with_suffixes(new_suffixes)
        } else {
            var_expr
        }
    }
}

/// Transforms Luau code by expanding alias requires defined in `.luaurc` and `limon.toml`.
/// Returns `(transformed_code, was_modified)` or error if parsing fails.
pub fn transform_luau(
    code: &str,
    valid_luaurc_aliases: &HashSet<String>,
    alias_expansions: &HashMap<String, String>,
) -> Result<(String, bool), Vec<full_moon::Error>> {
    if valid_luaurc_aliases.is_empty() || alias_expansions.is_empty() {
        return Ok((code.to_string(), false));
    }

    let ast = full_moon::parse(code)?;
    let mut replacer = RequireAliasReplacer::new(valid_luaurc_aliases, alias_expansions);
    let new_ast = replacer.visit_ast(ast);

    if replacer.modified {
        Ok((new_ast.to_string(), true))
    } else {
        Ok((code.to_string(), false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transform_requires() {
        let mut valid_aliases = HashSet::new();
        valid_aliases.insert("pkg".to_string());
        valid_aliases.insert("utils".to_string());

        let mut expansions = HashMap::new();
        expansions.insert(
            "pkg".to_string(),
            "@game/ReplicatedStorage/Packages".to_string(),
        );
        expansions.insert(
            "utils".to_string(),
            "@game/ReplicatedStorage/Utils".to_string(),
        );

        let input = r#"
-- comments should be preserved
local A = require("@pkg/Roact")
local B = require '@pkg'
local C = require "@game/ReplicatedStorage/Packages/Roact"
local D = require("@utils/Math").round
local E = require("@unknown/Something")
local F = other_function("@pkg/DoNotTouch")
local G = require "@pkg/DirectString"
local H = require("@pkg/Chain"):Init()
"#;

        let (output, modified) =
            transform_luau(input, &valid_aliases, &expansions).expect("transform failed");

        assert!(modified);
        assert!(output.contains(r#"local A = require("@game/ReplicatedStorage/Packages/Roact")"#));
        assert!(output.contains(r#"local B = require '@game/ReplicatedStorage/Packages'"#));
        assert!(output.contains(r#"local C = require "@game/ReplicatedStorage/Packages/Roact""#));
        assert!(output.contains(r#"local D = require("@game/ReplicatedStorage/Utils/Math").round"#));
        assert!(output.contains(r#"local E = require("@unknown/Something")"#));
        assert!(output.contains(r#"local F = other_function("@pkg/DoNotTouch")"#));
        assert!(output.contains(r#"local G = require "@game/ReplicatedStorage/Packages/DirectString""#));
        assert!(output.contains(r#"local H = require("@game/ReplicatedStorage/Packages/Chain"):Init()"#));
        assert!(output.contains("-- comments should be preserved"));
    }

    #[test]
    fn test_trailing_slash_normalization() {
        let mut valid_aliases = HashSet::new();
        valid_aliases.insert("pkg".to_string());

        let mut expansions = HashMap::new();
        expansions.insert(
            "pkg".to_string(),
            "@game/ReplicatedStorage/Packages/".to_string(),
        );

        let input = r#"local A = require("@pkg/Sub")"#;
        let (output, modified) =
            transform_luau(input, &valid_aliases, &expansions).expect("transform failed");
        assert!(modified);
        assert_eq!(
            output,
            r#"local A = require("@game/ReplicatedStorage/Packages/Sub")"#
        );
    }
}
