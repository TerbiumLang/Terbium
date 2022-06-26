#![feature(lint_reasons)]
#![feature(stmt_expr_attributes)]
#![feature(box_patterns)]

pub mod util;

use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter, Result as FmtResult};
use terbium_grammar::{Body, Expr, Node, Operator, ParseInterface, Source, Span, Spanned, Target, Token, TypeExpr};
use util::to_snake_case;

use crate::util::get_levenshtein_distance;
use ariadne::{sources, Cache, Color, Label, Report, ReportBuilder, ReportKind};
use std::io::Write;
use std::str::FromStr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnalyzerMessageKind {
    Info,
    Alert(AnalyzerKind),
}

pub struct AnalyzerMessage {
    pub kind: AnalyzerMessageKind,
    pub report: ReportBuilder<Span>,
    span: Span,
}

impl AnalyzerMessage {
    pub fn new<F>(kind: AnalyzerMessageKind, span: Span, f: F) -> Self
    where
        F: FnOnce(ReportBuilder<Span>, Color) -> ReportBuilder<Span>,
    {
        #[allow(
            clippy::match_wildcard_for_single_variants,
            reason = "Nothing should reach this arm"
        )]
        let color = match &kind {
            AnalyzerMessageKind::Info => Color::Blue,
            AnalyzerMessageKind::Alert(k) if k.is_warning() => Color::Yellow,
            AnalyzerMessageKind::Alert(k) if k.is_error() => Color::Red,
            _ => unreachable!(),
        };

        let report = Report::build(
            #[allow(
                clippy::match_wildcard_for_single_variants,
                reason = "Nothing should reach this arm"
            )]
            match &kind {
                AnalyzerMessageKind::Info => ReportKind::Advice,
                AnalyzerMessageKind::Alert(k) if k.is_warning() => ReportKind::Warning,
                AnalyzerMessageKind::Alert(k) if k.is_error() => ReportKind::Error,
                _ => unreachable!(),
            },
            span.src(),
            span.start(),
        );

        Self {
            kind,
            span,
            report: f(report, color),
        }
    }

    #[must_use]
    pub fn non_snake_case(name: &str, counterpart: String, span: Span) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::NonSnakeCase),
            span.clone(),
            |report, color| report
                .with_message("non-type identifier names should be snake_case")
                .with_label(Label::new(span)
                    .with_message(format!("{:?} is not snake_case", name))
                    .with_color(color)
                )
                .with_help(format!("rename to {:?}", counterpart))
        )
    }

    #[must_use]
    pub fn unnecessary_mut_variable(name: &str, span: Span) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::UnnecessaryMutVariables),
            span.clone(),
            |report, color| report
                .with_message("variable was unneedingly declared as mutable")
                .with_label(Label::new(span)
                    .with_message(format!(
                        "variable {:?} declared mutable here, but it is never mutated",
                        name,
                    ))
                    .with_color(color)
                )
                .with_help("make variable immutable by declaring with `let` instead")
        )
    }

    #[must_use]
    pub fn unused_variable(name: &str, span: Span) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::UnusedVariables),
            span.clone(),
            |report, color| report
                .with_message("variable is declared but never used")
                .with_label(Label::new(span)
                    .with_message(format!(
                        "variable {:?} is declared here, but it is never used",
                        name,
                    ))
                    .with_color(color)
                )
                .with_help(format!(
                    "remove the declaration, or prefix with an underscore: {:?}",
                    "_".to_string() + name,
                ))
        )
    }

    #[must_use]
    pub fn global_mutable_variable(span: Span) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::GlobalMutableVariables),
            span.clone(),
            |report, color| report
                .with_message("mutable declaration found in the global scope")
                .with_label(Label::new(span)
                    .with_message("variables declared here are accessible to the entire program")
                    .with_color(color)
                )
                .with_help(
                    "declare as immutable instead, or move the declaration into a \
                    non-global context such as inside of a function"
                )
        )
    }

    #[must_use]
    pub fn unresolved_identifier(name: &str, close_match: Option<(String, Span)>, span: Span) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::UnresolvedIdentifiers),
            span.clone(),
            |report, color| {
                let report = report
                    .with_message("identifier could not be resolved")
                    .with_label(Label::new(span)
                        .with_message(format!("variable {:?} not found in this scope", name))
                        .with_color(color)
                        .with_order(0)
                    );

                if let Some((close_match, close_span)) = close_match {
                    report.with_label(Label::new(close_span)
                        .with_message(format!("perhaps you meant {:?}, which was declared here", close_match))
                        .with_color(Color::Cyan)
                        .with_order(1)
                    )
                } else {
                    report
                }
            },
        )
    }

    #[must_use]
    pub fn redeclared_const_variable(name: &str, decl_span: Span, span: Span) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::RedeclaredConstVariables),
            span.clone(),
            |report, color| report
                .with_message("cannot redeclare variable declared as `const`")
                .with_label(Label::new(decl_span)
                    .with_message(format!("variable {:?} declared as `const` here", name))
                    .with_color(color)
                    .with_order(0))
                .with_label(Label::new(span)
                    .with_message(format!("attempted to redeclare {:?} here", name))
                    .with_color(color)
                    .with_order(1))
                .with_help("declare with `let` instead")
        )
    }

    #[must_use]
    pub fn reassigned_immutable_variable(name: &str, decl_span: Span, span: Span, was_const: bool) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::ReassignedImmutableVariables),
            span.clone(),
            |report, color| report
                .with_message(format!(
                    "cannot reassign to {}",
                    if was_const {
                        "variable declared as `const`"
                    } else {
                        "immutable variable"
                    }
                ))
                .with_label(Label::new(decl_span)
                    .with_message(format!(
                        "variable {:?} declared as {} here",
                        name,
                        if was_const { "`const`" } else { "immutable" },
                    ))
                    .with_color(Color::Cyan)
                    .with_order(0))
                .with_label(Label::new(span)
                    .with_message(format!("attempted to reassign to variable {:?} here", name))
                    .with_color(color)
                    .with_order(1))
                .with_help("make variable mutable by declaring with `let mut` instead")
        )
    }

    #[must_use]
    pub fn unsupported_unary_operator(
        span: Span,
        val_ty: String,
        val_span: Span,
        op: Operator,
        op_span: Span,
    ) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::UnsupportedOperators),
            span.clone(),
            |report, color| report
                .with_message(format!("type does not support unary {:?} operator", op.to_string()))
                .with_label(Label::new(val_span)
                    .with_message(format!("this is of type {}", val_ty))
                    .with_color(Color::Cyan)
                    .with_order(0)
                )
                .with_label(Label::new(op_span)
                    .with_message(format!(
                        "cannot use operator {:?} on {}",
                        op.to_string(),
                        val_ty,
                    ))
                    .with_color(color)
                    .with_order(1)
                )
                .with_help("try casting to a supported type")
        )
    }

    #[must_use]
    pub fn unsupported_binary_operator(
        span: Span,
        lhs_ty: String,
        lhs_span: Span,
        rhs_ty: String,
        rhs_span: Span,
        op: Operator,
        op_span: Span,
    ) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::UnsupportedOperators),
            span.clone(),
            |report, color| report
                .with_message(format!("these types do not support {:?} operator", op.to_string()))
                .with_label(Label::new(lhs_span)
                    .with_message(format!("this is of type {}", lhs_ty))
                    .with_color(Color::Cyan)
                    .with_order(0)
                )
                .with_label(Label::new(rhs_span)
                    .with_message(format!("this is of type {}", rhs_ty))
                    .with_color(Color::Cyan)
                    .with_order(1)
                )
                .with_label(Label::new(op_span)
                    .with_message(format!(
                        "cannot use operator {:?} on {} and {}",
                        op.to_string(),
                        lhs_ty,
                        rhs_ty,
                    ))
                    .with_color(color)
                    .with_order(2)
                )
                .with_help("try casting to supported types")
        )
    }

    #[must_use]
    pub fn unbalanced_if_statement(
        span: Span,
        first_span: Span,
        first_ty: String,
        second_span: Span,
        second_ty: String,
    ) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::UnbalancedIfStatements),
            span.clone(),
            |report, color| report
                .with_message("return types of if-statement are unbalanced")
                .with_label(Label::new(first_span)
                    .with_message(format!("this resolves to {}", first_ty))
                    .with_color(Color::Cyan)
                    .with_order(0)
                )
                .with_label(Label::new(second_span)
                    .with_message(format!("this resolves to {}, which is incompatible with {}", second_ty, first_ty))
                    .with_color(color)
                    .with_order(1)
                )
                .with_help("try adding semicolons or balancing the types")
        )
    }

    #[must_use]
    pub fn unbalanced_if_statement_no_else(
        span: Span,
        first_span: Span,
        first_ty: String,
    ) -> Self {
        Self::new(
            AnalyzerMessageKind::Alert(AnalyzerKind::UnbalancedIfStatements),
            span.clone(),
            |report, color| report
                .with_message("return types of if-statement are unbalanced")
                .with_label(Label::new(first_span)
                    .with_message(format!("this resolves to {}, which is not null", first_ty))
                    .with_color(color)
                    .with_order(0)
                )
                .with_label(Label::new(span)
                    .with_message("note that the lack of an `else` causes the possibility of null")
                    .with_color(color)
                    .with_order(1))
                .with_help("try adding semicolons")
        )
    }

    /// Write error to specified writer.
    ///
    /// # Panics
    /// * Panic when writing to writer failed.
    pub fn write<C: Cache<Source>>(self, cache: C, writer: impl Write) {
        let report = if let AnalyzerMessageKind::Alert(k) = self.kind {
            self.report
                .with_code(k.code())
                .with_note(format!(
                   "view this {} in the error index: \
                   https://github.com/TerbiumLang/standard/blob/main/error_index.md#{}{:0>3}",
                   if k.is_error() { "error" } else { "warning" },
                   if k.is_error() { "E" } else { "W" },
                   k.code(),
               ))
        } else {
            self.report
        };

        report.finish().write(cache, writer).unwrap();
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PrimitiveType {
    Int,
    Float,
    String,
    Bool,
}

impl Display for PrimitiveType {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Int => write!(f, "int"),
            Self::Float => write!(f, "float"),
            Self::String => write!(f, "string"),
            Self::Bool => write!(f, "bool"),
        }
    }
}

impl PrimitiveType {
    pub fn get_unary_op_outcome(&self, op: Operator) -> Option<Type> {
        type Op = Operator;

        Some(match (op, self) {
            (Op::Not, _) => Type::Primitive(Self::Bool),
            (Op::Add | Op::Sub, t @ Self::Int | Self::Float) => Type::Primitive(*t),
            (Op::BitNot, Self::Int) => Type::Primitive(Self::Int),
            _ => return None,
        })
    }

    pub fn get_binary_op_outcome(&self, op: Operator, other: &Type) -> Option<Type> {
        type Op = Operator;

        Some(match (self, op, other) {
            (
                Self::Int,
                Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Pow | Op::Mod | Op::BitOr | Op::BitAnd | Op::BitXor | Op::BitLShift | Op::BitRShift,
                Type::Primitive(Self::Int),
            ) => Type::Primitive(Self::Int),
            (
                Self::Float | Self::Int,
                Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Pow | Op::Mod,
                Type::Primitive(Self::Float | Self::Int)
            ) => Type::Primitive(Self::Float),
            (
                Self::Float | Self::Int,
                Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Eq | Op::Ne,
                Type::Primitive(Self::Float | Self::Int),
            ) => Type::Primitive(Self::Bool),
            (Self::String, Op::Add | Op::Mul, Type::Primitive(Self::String)) => Type::Primitive(Self::String),
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
/// A struct that stores instructions on how to resolve a type.
pub enum DeferredType {
    /// Reference this entry and retrieve its type after it is resolved
    TypeOf(*const MockScopeEntry),

    /// A substitute for a known type, but when required as a deferred type.
    ///
    /// An example is having to apply a binary operator to a deferred type and a known type.
    /// Despite the known type, this type is still deferred.
    Known(Type),

    ApplyUnary(Operator, Box<Self>),
    ApplyBinary(Operator, Box<Self>, Box<Self>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    Primitive(PrimitiveType),
    Union(Box<Self>, Box<Self>),
    And(Box<Self>, Box<Self>),
    Array(Box<Self>, Option<u32>),
    Tuple(Vec<Self>),
    Func(Vec<Type>, Box<Type>),
    Null,
    Any,

    // Eventually will be resolved
    Deferred(DeferredType),
    Unknown,
}

impl Type {
    pub fn get_unary_op_outcome(&self, op: Operator) -> Option<Self> {
        Some(match (op, self) {
            (op, Self::Deferred(d)) => Self::Deferred(
                DeferredType::ApplyUnary(op, Box::new(d.clone())),
            ),
            (op, Self::Primitive(p)) => p.get_unary_op_outcome(op)?,
            (op, Self::Union(a, b)) =>
                a.get_unary_op_outcome(op).and(b.get_unary_op_outcome(op))?,
            (op, Self::And(a, b)) =>
                a.get_unary_op_outcome(op).or(b.get_unary_op_outcome(op))?,
            (Operator::Not, _) => Self::Primitive(PrimitiveType::Bool),
            (_, t @ (Self::Any | Self::Unknown)) => t.clone(),
            _ => return None,
        })
    }

    pub fn get_binary_op_outcome(&self, op: Operator, other: &Type) -> Option<Self> {
        Some(match (self, op, other) {
            (Self::Deferred(a), op, Self::Deferred(b)) => Self::Deferred(
                DeferredType::ApplyBinary(op, Box::new(a.clone()), Box::new(b.clone()))
            ),
            (Self::Primitive(a), op, b) => a.get_binary_op_outcome(op, b),
            (Self::Union(box a, box b), op, c)
            | (c, op, Self::Union(box a, box b)) => {
                a.get_binary_op_outcome(op, c)
                    .and(b.get_binary_op_outcome(op, c))?
            },
            (Self::And(box a, box b), op, c)
            | (c, op, Self::And(box a, box b)) => {
                a.get_binary_op_outcome(op, c)
                    .or(b.get_binary_op_outcome(op, c))?
            },
            (Self::Array(ty_a, len_a), Operator::Add, Self::Array(ty_b, len_b)) => {
                Self::Array(
                    Box::new(Self::Union(ty_a.clone(), ty_b.clone())),
                    len_a.map(|l| l + len_b.unwrap_or(0)),
                )
            }
            (Self::Tuple(a), Operator::Add, Self::Tuple(b)) => {
                Self::Tuple(a.clone().into_iter().chain(b.clone()).collect())
            }
            (Self::Any, _, _) | (_, _, Self::Any) => Self::Any,
            (Self::Unknown, _, _) | (_, _, Self::Unknown) => Self::Unknown,
            _ => return None,
        })
    }

    pub fn flatten(self) -> Self {
        match self {
            Self::Union(a, b)
            | Self::And(a, b)
            if a == b => a.flatten(),
            o => o,
        }
    }
}

impl Display for Type {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Primitive(p) => write!(f, "{}", p),
            Self::Union(box Self::Null, ty) | Self::Union(ty, box Self::Null)
                => write!(f, "?{}", ty),
            Self::Union(lhs, rhs) => write!(f, "{} | {}", lhs, rhs),
            Self::And(lhs, rhs) => write!(f, "{} & {}", lhs, rhs),
            Self::Array(ty, size) => write!(f, "{}[{}]", ty, size
                .map(ToString::to_string)
                .unwrap_or_else(String::new)),
            Self::Tuple(items) => write!(f, "[{}]", items
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")),
            Self::Func(params, ret) => write!(
                f,
                "({}) -> {}",
                params
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                ret,
            ),
            Self::Null => write!(f, "null"),
            Self::Any => write!(f, "any"),
            Self::Deferred(_) => write!(f, "<unknown>"),
            Self::Unknown => write!(f, "<unknown>"),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScopeEntryModifier {
    None,
    Mut,
    Const,
}

#[derive(Clone, Debug)]
pub struct MockScopeEntry {
    pub name: String,
    pub ty: Type,
    pub modifier: ScopeEntryModifier,
    pub used: bool,
    pub mutated: bool,
    pub span: Span,
}

impl MockScopeEntry {
    #[must_use]
    pub const fn new(name: String, ty: Type, modifier: ScopeEntryModifier, span: Span) -> Self {
        Self {
            name,
            ty,
            modifier,
            used: false,
            mutated: false,
            span,
        }
    }

    #[must_use]
    pub fn is_let(&self) -> bool {
        self.modifier == ScopeEntryModifier::None
    }

    #[must_use]
    pub fn is_mut(&self) -> bool {
        self.modifier == ScopeEntryModifier::Mut
    }

    #[must_use]
    pub fn is_const(&self) -> bool {
        self.modifier == ScopeEntryModifier::Const
    }

    #[must_use]
    pub const fn is_mutated(&self) -> bool {
        self.mutated
    }
}

#[derive(Debug)]
pub struct MockScope(pub HashMap<String, MockScopeEntry>);

impl MockScope {
    #[must_use]
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&MockScopeEntry> {
        self.0.get(name)
    }
}

impl Default for MockScope {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Context {
    pub tokens: Vec<(Token, Span)>,
    pub ast: Node,
    pub messages: Vec<AnalyzerMessage>,
    pub scopes: Vec<MockScope>,
    pub cache: Vec<(Source, String)>,
}

impl Context {
    #[must_use]
    pub fn from_tokens(cache: Vec<(Source, String)>, tokens: Vec<(Token, Span)>) -> Self {
        let ast = Node::parse(tokens.clone()).unwrap_or_else(|e| {
            for error in e {
                error.write(sources(cache.clone()), std::io::stderr());
            }

            std::process::exit(-1)
        });

        Self {
            tokens,
            ast,
            messages: Vec::new(),
            scopes: vec![MockScope::new()],
            cache,
        }
    }

    #[must_use]
    pub fn cache(&self) -> impl Cache<Source> {
        sources(self.cache.clone())
    }

    #[must_use]
    pub fn is_top_level(&self) -> bool {
        self.scopes.len() == 1
    }

    #[must_use]
    pub fn locals(&self) -> &MockScope {
        self.scopes.last().unwrap_or_else(|| unreachable!())
    }

    #[must_use]
    pub fn locals_mut(&mut self) -> &mut MockScope {
        self.scopes.last_mut().unwrap_or_else(|| unreachable!())
    }

    pub fn store_var(&mut self, name: String, entry: MockScopeEntry) {
        self.locals_mut().0.insert(name, entry);
    }

    #[must_use]
    pub fn lookup_var(&self, name: &String) -> Option<&MockScopeEntry> {
        for scope in self.scopes.iter().rev() {
            if let Some(entry) = scope.0.get(name) {
                return Some(entry);
            }
        }

        None
    }

    #[must_use]
    pub fn lookup_var_mut(&mut self, name: &String) -> Option<&mut MockScopeEntry> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(entry) = scope.0.get_mut(name) {
                return Some(entry);
            }
        }

        None
    }

    #[must_use]
    pub fn close_var_match(&self, name: &str) -> Option<(String, Span)> {
        #[allow(
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            reason = "It isn't possible for an indentifer to be this large"
        )]
        let threshold = (name.chars().count() as f64 * 0.14).round().max(2_f64) as usize;

        for scope in self.scopes.iter().rev() {
            for (sample, entry) in &scope.0 {
                if get_levenshtein_distance(name, sample.as_str()) <= threshold {
                    return Some((sample.clone(), entry.span.clone()));
                }
            }
        }

        None
    }

    pub fn enter_scope(&mut self) {
        self.scopes.push(MockScope::new());
    }

    pub fn exit_scope(&mut self, analyzers: &AnalyzerSet, messages: &mut Vec<AnalyzerMessage>) {
        let scope = self.scopes.pop().unwrap_or_else(|| unreachable!());

        for entry in scope.0.into_values() {
            if analyzers.contains(&AnalyzerKind::UnnecessaryMutVariables)
                && entry.is_mut()
                && !entry.is_mutated()
            {
                messages.push(AnalyzerMessage::unnecessary_mut_variable(
                    &entry.name,
                    entry.span.clone(),
                ));
            }

            if analyzers.contains(&AnalyzerKind::UnusedVariables)
                && !entry.used
                && !entry.name.starts_with('_')
            {
                messages.push(AnalyzerMessage::unused_variable(&entry.name, entry.span));
            }
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AnalyzerKind {
    /// [W000] Non-type identifier names should be snake_case
    NonSnakeCase,
    /// [W001] Type identifier names, such as classes or traits should be PascalCase
    NonPascalCase,
    /// [W002] Identifier names should contain only ASCII characters
    NonAscii,
    /// [W003] A variable or parameter was declared but never used
    UnusedVariables,
    /// [W004] A variable or parameter was declared as mutable, but is never mutated
    UnnecessaryMutVariables,
    /// [W005] Global mutable variables are highly discouraged
    GlobalMutableVariables,
    /// [W006] Value of an if statement has unbalanced types
    UnbalancedIfStatements,
    /// [E001] An identifier (e.g. a variable) could not be found in the current scope
    UnresolvedIdentifiers,
    /// [E002] A variable declared as `const` was redeclared later on
    RedeclaredConstVariables,
    /// [E003] An immutable variable was reassigned to
    ReassignedImmutableVariables,
    /// [E004] The operator is not supported for the given type
    UnsupportedOperators,
    /// [E005] Received a type that was incompatible with what was expected
    IncompatibleTypes,
    /// [E006] The type could not be inferred
    UninferableTypes,
}

impl AnalyzerKind {
    /// Returns a number 1 to 5 (inclusive) representing
    /// the servity of this specific type of warning.
    ///
    /// This can be used to ignore errors lower than a specific severity,
    /// or exit the analysis stage all together when a warning with a higher
    /// serverity is encountered.
    ///
    /// A higher number means a more severe warning.
    /// By default, the analyzer is set to ignore no errors and stop
    /// analysis at only level 5.
    ///
    /// If this is an error, return 0.
    #[must_use]
    pub const fn severity(&self) -> u8 {
        match self {
            Self::NonSnakeCase | Self::NonPascalCase | Self::NonAscii => 1,
            Self::UnusedVariables | Self::UnnecessaryMutVariables => 2,
            Self::UnbalancedIfStatements => 3,
            Self::GlobalMutableVariables => 4,
            Self::UnresolvedIdentifiers
            | Self::RedeclaredConstVariables
            | Self::ReassignedImmutableVariables
            | Self::UnsupportedOperators
            | Self::IncompatibleTypes
            | Self::UninferableTypes => 0,
        }
    }

    /// References the error index
    #[must_use]
    pub const fn code(&self) -> u8 {
        match self {
            Self::NonSnakeCase => 0,
            Self::NonPascalCase | Self::UnresolvedIdentifiers => 1,
            Self::NonAscii | Self::RedeclaredConstVariables => 2,
            Self::UnusedVariables | Self::ReassignedImmutableVariables => 3,
            Self::UnnecessaryMutVariables | Self::UnsupportedOperators => 4,
            Self::GlobalMutableVariables | Self::IncompatibleTypes => 5,
            Self::UnbalancedIfStatements | Self::UninferableTypes => 6,
        }
    }

    #[must_use]
    pub const fn is_warning(&self) -> bool {
        self.severity() != 0
    }

    #[must_use]
    pub const fn is_error(&self) -> bool {
        self.severity() == 0
    }

    #[must_use]
    pub const fn warn_level(&self) -> Option<u8> {
        match self.severity() {
            0 => None,
            n => Some(n),
        }
    }
}

impl FromStr for AnalyzerKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "non-snake-case" => Self::NonSnakeCase,
            "non-pascal_case" => Self::NonPascalCase,
            "non-ascii" => Self::NonAscii,
            "unused-variables" => Self::UnusedVariables,
            "unnecessary-mut-variables" => Self::UnnecessaryMutVariables,
            "unresolved-identifiers" => Self::UnresolvedIdentifiers,
            "redeclared-const-variables" => Self::RedeclaredConstVariables,
            "reassigned-immutable-variables" => Self::ReassignedImmutableVariables,
            "global-mutable-variables" => Self::GlobalMutableVariables,
            "unsupported-operators" => Self::UnsupportedOperators,
            "incompatible-types" => Self::IncompatibleTypes,
            "uninferable-types" => Self::UninferableTypes,
            "unbalanced-if-statements" => Self::UnbalancedIfStatements,
            _ => return Err(format!("invalid analyzer {:?}", s)),
        })
    }
}

impl std::fmt::Display for AnalyzerKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NonSnakeCase => "non-snake-case",
            Self::NonPascalCase => "non-pascal_case",
            Self::NonAscii => "non-ascii",
            Self::UnusedVariables => "unused-variables",
            Self::UnnecessaryMutVariables => "unnecessary-mut-variables",
            Self::UnresolvedIdentifiers => "unresolved-identifiers",
            Self::RedeclaredConstVariables => "redeclared-const-variables",
            Self::ReassignedImmutableVariables => "reassigned-immutable-variables",
            Self::GlobalMutableVariables => "global-mutable-variables",
            Self::UnsupportedOperators => "unsupported-operators",
            Self::IncompatibleTypes => "incompatible-types",
            Self::UninferableTypes => "uninferable-types",
            Self::UnbalancedIfStatements => "unbalanced-if-statements",
        })
    }
}

#[derive(Clone, Debug)]
pub struct AnalyzerSet(pub HashSet<AnalyzerKind>);

impl AnalyzerSet {
    #[must_use]
    pub fn contains(&self, member: &AnalyzerKind) -> bool {
        self.0.contains(member)
    }

    #[must_use]
    pub fn none() -> Self {
        Self(HashSet::new())
    }

    #[must_use]
    pub fn all() -> Self {
        type A = AnalyzerKind;

        Self(HashSet::from([
            A::NonSnakeCase,
            A::NonPascalCase,
            A::NonAscii,
            A::UnusedVariables,
            A::UnnecessaryMutVariables,
            A::UnresolvedIdentifiers,
            A::RedeclaredConstVariables,
            A::ReassignedImmutableVariables,
            A::GlobalMutableVariables,
            A::UnsupportedOperators,
            A::IncompatibleTypes,
            A::UninferableTypes,
            A::UnbalancedIfStatements,
        ]))
    }

    #[must_use]
    pub fn from_disabled(disabled: &HashSet<AnalyzerKind>) -> Self {
        Self(Self::default().0.difference(disabled).copied().collect())
    }

    #[must_use]
    pub fn from_allowed_disabled(
        allowed: &HashSet<AnalyzerKind>,
        disabled: &HashSet<AnalyzerKind>,
    ) -> Self {
        Self(
            Self::default()
                .0
                .union(allowed)
                .collect::<HashSet<_>>()
                .difference(&disabled.iter().collect())
                .map(|a| **a)
                .collect::<HashSet<_>>(),
        )
    }
}

impl Default for AnalyzerSet {
    fn default() -> Self {
        Self::all()
    }
}

pub fn infer_type(
    ctx: &Context,
    messages: &mut Vec<AnalyzerMessage>,
    expr: &Spanned<Expr>,
) -> Result<Type, &'static str> {
    let (expr, span) = expr.node_span();

    Ok(match expr {
        Expr::Integer(_) => Type::Primitive(PrimitiveType::Int),
        Expr::Float(_) => Type::Primitive(PrimitiveType::Float),
        Expr::String(_) => Type::Primitive(PrimitiveType::String),
        Expr::Bool(_) => Type::Primitive(PrimitiveType::Bool),
        Expr::UnaryExpr { operator, value } => {
            let (op, op_span) = operator.node_span();
            let t = infer_type(ctx, messages, value)?;

            match t.get_unary_op_outcome(*op) {
                Some(ty) => ty,
                None => {
                    messages.push(AnalyzerMessage::unsupported_unary_operator(
                        span.clone(),
                        t.to_string(),
                        value.span(),
                        *op,
                        op_span.clone(),
                    ));

                    Type::Unknown
                }
            }
        }
        Expr::BinaryExpr { operator, lhs, rhs } => {
            let (op, op_span) = operator.node_span();
            let lhs_t = infer_type(ctx, messages, lhs)?;
            let rhs_t = infer_type(ctx, messages, rhs)?;

            match lhs_t.get_binary_op_outcome(*op, &rhs_t) {
                Some(ty) => ty,
                None => {
                    messages.push(AnalyzerMessage::unsupported_binary_operator(
                        span.clone(),
                        lhs_t.to_string(),
                        lhs.span(),
                        rhs_t.to_string(),
                        rhs.span(),
                        *op,
                        op_span.clone(),
                    ));

                    Type::Unknown
                }
            }
        }
        Expr::If { body, else_if_bodies, else_body, .. } => {
            let mut bodies = else_if_bodies.iter().map(|b| &b.1).collect::<Vec<_>>();
            bodies.insert(0, body);

            if let Some(else_body) = else_body {
                bodies.push(else_body)
            }

            let mut types = bodies.into_iter().map(|s| {
                let (
                    Body(nodes, return_last),
                    body_span
                ) = s.node_span();

                let ty = if return_last {
                    if let Node::Expr(e) = nodes.last().unwrap() {
                        infer_type(ctx, messages, e)?
                    } else {
                        Type::Null
                    }
                } else {
                    Type::Null
                };

                (ty, body_span)
            });

            let (target_type, first_span) = types.next().unwrap();
            let accumulator = target_type.clone();

            for (subject_type, subject_span) in types {

            }
        }
    })
}

#[allow(unused_variables, reason = "`analyzers` will be used later")]
/// Analyzes the expression.
///
/// # Errors
/// * The analyzer generated an error.
pub fn visit_expr(
    analyzers: &AnalyzerSet,
    ctx: &mut Context,
    messages: &mut Vec<AnalyzerMessage>,
    expr: Spanned<Expr>,
) -> Result<(), &'static str> {
    let span = expr.span();
    let expr = expr.into_node();

    match expr {
        Expr::Ident(s) => match ctx.lookup_var_mut(&s) {
            Some(e) => {
                e.used = true;
            }
            None => {
                let close_match = ctx.close_var_match(&s);

                messages.push(AnalyzerMessage::unresolved_identifier(
                    &s,
                    close_match,
                    span,
                ));
            }
        },
        Expr::If {
            condition,
            body,
            mut else_if_bodies,
            else_body,
        } => {
            else_if_bodies.insert(0, (condition, body));

            for (condition, body) in else_if_bodies {
                visit_expr(analyzers, ctx, messages, condition)?;

                ctx.enter_scope();
                for node in body.into_node().0 {
                    visit_node(analyzers, ctx, messages, node)?;
                }
                ctx.exit_scope(analyzers, messages);
            }

            if let Some(else_body) = else_body {
                ctx.enter_scope();
                for node in else_body.into_node().0 {
                    visit_node(analyzers, ctx, messages, node)?;
                }
                ctx.exit_scope(analyzers, messages);
            }
        }
        Expr::While { condition, body } => {
            visit_expr(analyzers, ctx, messages, condition)?;

            ctx.enter_scope();
            for node in body {
                visit_node(analyzers, ctx, messages, node)?;
            }
            ctx.exit_scope(analyzers, messages);
        }
        Expr::UnaryExpr { operator, value } => {
            visit_expr(analyzers, ctx, messages, value)?;
        }
        Expr::BinaryExpr { operator, lhs, rhs } => {
            visit_expr(analyzers, ctx, messages, lhs)?;
            visit_expr(analyzers, ctx, messages, rhs)?;
        }
        _ => return Ok(()),
    }

    Ok(())
}

#[allow(clippy::missing_panics_doc, reason = "todo!()")]
#[allow(clippy::too_many_lines, reason = "Should refactor later.")]
/// Analyzes the node.
///
/// # Errors
/// * The analyzer generated an error.
pub fn visit_node(
    analyzers: &AnalyzerSet,
    ctx: &mut Context,
    messages: &mut Vec<AnalyzerMessage>,
    node: Spanned<Node>,
) -> Result<(), &'static str> {
    let span = node.span();
    let node = node.into_node();

    match node {
        Node::Module(m) => {
            for node in m {
                visit_node(analyzers, ctx, messages, node)?;
            }
        }
        Node::Declare {
            targets,
            ty,
            r#mut,
            r#const,
            value,
        } => {
            type DeferEntry = (String, (), Span);

            fn recur(
                ctx: &Context,
                messages: &mut Vec<AnalyzerMessage>,
                target: Target,
                span: Span,
                tgt_span: Span,
                deferred: &mut Vec<DeferEntry>,
            ) {
                #[allow(clippy::match_wildcard_for_single_variants, reason = "todo!()")]
                match target {
                    Target::Ident(s) => {
                        if let Some(entry) = ctx.lookup_var(&s) {
                            if entry.is_const() {
                                messages.push(AnalyzerMessage::redeclared_const_variable(&s, entry.span.clone(), span));
                            }
                        }

                        deferred.push((s, (), tgt_span));
                    }
                    Target::Array(targets) => {
                        for target in targets {
                            let (target, tgt_span) = target.into_node_span();

                            recur(
                                ctx,
                                messages,
                                target,
                                span.clone(),
                                tgt_span.clone(),
                                deferred,
                            );
                        }
                    }
                    _ => todo!(),
                };
            }

            // Assume there can only be one target
            let (target, tgt_span) = targets
                .first()
                .ok_or("multiple declaration targets unsupported")?
                .node_span();

            let modifier = match (r#mut, r#const) {
                (true, false) => ScopeEntryModifier::Mut,
                (false, true) => ScopeEntryModifier::Const,
                (false, false) => ScopeEntryModifier::None,
                (true, true) => unreachable!(),
            };

            if analyzers.contains(&AnalyzerKind::GlobalMutableVariables)
                && ctx.is_top_level()
                && modifier == ScopeEntryModifier::Mut
            {
                messages.push(AnalyzerMessage::global_mutable_variable(
                    span.clone(),
                ));
            }

            let mut deferred = Vec::<DeferEntry>::new();

            recur(
                ctx,
                messages,
                target.clone(),
                span.clone(),
                tgt_span.clone(),
                &mut deferred,
            );

            for (name, ty, tgt_span) in deferred {
                if analyzers.contains(&AnalyzerKind::NonSnakeCase) {
                    let snake = to_snake_case(&*name);

                    if name != snake {
                        messages.push(AnalyzerMessage::non_snake_case(
                            &name,
                            snake,
                            tgt_span.clone(),
                        ));
                    }
                }

                ctx.store_var(
                    name.clone(),
                    MockScopeEntry::new(name, ty, modifier, span.clone()),
                );
            }
        }
        Node::Assign { targets, .. } => {
            // Assume there can only be one target
            let (target, tgt_span) = targets
                .first()
                .ok_or("multiple assignment targets unsupported")?
                .node_span();

            #[allow(clippy::match_wildcard_for_single_variants, reason = "todo!()")]
            match target {
                Target::Ident(s) => {
                    let entry = ctx.lookup_var_mut(s);

                    match entry {
                        Some(entry) => {
                            entry.mutated = true;

                            if entry.is_const() || !entry.is_mut() {
                                messages.push(AnalyzerMessage::reassigned_immutable_variable(
                                    s,
                                    entry.span.clone(),
                                    span,
                                    entry.is_const(),
                                ));
                            }
                        }
                        None => {
                            let close_match = ctx.close_var_match(s);

                            messages.push(AnalyzerMessage::unresolved_identifier(
                                s,
                                close_match,
                                tgt_span.clone(),
                            ));
                            return Ok(());
                        }
                    }
                }
                Target::Array(_) => return Err("array assignments unsupported"),
                _ => todo!(),
            }
        }
        Node::Expr(expr) => visit_expr(analyzers, ctx, messages, expr)?,
        _ => unimplemented!(),
    }

    Ok(())
}

/// Analyze the given context.
///
/// # Errors
/// * Return any warning or error the analyzer generated.
pub fn run_analysis(
    analyzers: &AnalyzerSet,
    mut ctx: Context,
) -> Result<Vec<AnalyzerMessage>, &'static str> {
    let mut messages = Vec::new();
    let ast = std::mem::replace(&mut ctx.ast, Node::Module(Vec::new()));

    visit_node(
        analyzers,
        &mut ctx,
        &mut messages,
        Spanned::new(
            ast,
            Span::default(), // Guaranteed to be a module, this is a placeholder
        ),
    )?;

    ctx.exit_scope(analyzers, &mut messages);
    Ok(messages)
}
