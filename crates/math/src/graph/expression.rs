//! Graph の式（`x` の 1 変数）の compile と評価。中立な数値 compiler で、Python も TeX も使わない。
//!
//! 文法: 和（`+` `-`）・積（`*` `/`）・単項の符号・冪（`^`、右結合）・括弧・数値（`1.5e-3` など）・
//! `x` `pi` `e`・関数 `sin` `cos` `tan` `exp` `log` `sqrt` `abs`。入力の大きさ・再帰・深さ・node 数に上限を持つ。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Literal,
    X,
    Pi,
    E,
    Positive,
    Negative,
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
    Sin,
    Cos,
    Tan,
    Exp,
    Log,
    Sqrt,
    Abs,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Node {
    operation: Operation,
    literal: f64,
    left: Option<usize>,
    right: Option<usize>,
    depth: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Expression {
    nodes: Vec<Node>,
    root: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileStatus {
    Success,
    InvalidExpression,
    UnsupportedExpression,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileLimit {
    None,
    SourceBytes,
    Recursion,
    Depth,
    Nodes,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Compilation {
    pub status: CompileStatus,
    pub limit: CompileLimit,
    pub offset: usize,
    pub expression: Expression,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvaluationStatus {
    Finite,
    UndefinedDivision,
    UndefinedDomain,
    UndefinedOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Evaluation {
    pub status: EvaluationStatus,
    pub value: f64,
}

const MAXIMUM_SOURCE_BYTES: usize = 4096;
const MAXIMUM_NODES: usize = 4096;
const MAXIMUM_DEPTH: usize = 64;

struct Parser<'a> {
    source: &'a [u8],
    text: &'a str,
    position: usize,
    budget: usize,
    status: CompileStatus,
    limit: CompileLimit,
    nodes: Vec<Node>,
}

impl Parser<'_> {
    fn fail(&mut self, limit: CompileLimit) {
        if self.status == CompileStatus::Success {
            self.status = CompileStatus::InvalidExpression;
            self.limit = limit;
        }
    }

    fn ok(&self) -> bool {
        self.status == CompileStatus::Success
    }

    fn skip(&mut self) {
        while self
            .source
            .get(self.position)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.position += 1;
        }
    }

    fn take(&mut self, byte: u8) -> bool {
        self.skip();
        if self.source.get(self.position) == Some(&byte) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    /// 入力で増える再帰だけを数える。
    fn descend(&mut self, depth: usize) -> bool {
        if depth >= MAXIMUM_DEPTH {
            self.fail(CompileLimit::Recursion);
            return false;
        }
        self.ok()
    }

    fn node(
        &mut self,
        operation: Operation,
        left: Option<usize>,
        right: Option<usize>,
        literal: f64,
    ) -> Option<usize> {
        if !self.ok() {
            return None;
        }
        let depth_of = |index: Option<usize>, nodes: &[Node]| {
            index
                .and_then(|index| nodes.get(index))
                .map_or(0, |node| node.depth)
        };
        let depth = 1 + depth_of(left, &self.nodes).max(depth_of(right, &self.nodes));
        if depth > MAXIMUM_DEPTH {
            self.fail(CompileLimit::Depth);
            return None;
        }
        if self.nodes.len() >= self.budget {
            self.fail(CompileLimit::Nodes);
            return None;
        }
        self.nodes.push(Node {
            operation,
            literal,
            left,
            right,
            depth,
        });
        Some(self.nodes.len() - 1)
    }

    fn sum(&mut self, depth: usize) -> Option<usize> {
        let mut left = self.product(depth);
        while self.ok() {
            if self.take(b'+') {
                let right = self.product(depth);
                left = self.node(Operation::Add, left, right, 0.0);
            } else if self.take(b'-') {
                let right = self.product(depth);
                left = self.node(Operation::Subtract, left, right, 0.0);
            } else {
                break;
            }
        }
        left
    }

    fn product(&mut self, depth: usize) -> Option<usize> {
        let mut left = self.unary(depth);
        while self.ok() {
            if self.take(b'*') {
                let right = self.unary(depth);
                left = self.node(Operation::Multiply, left, right, 0.0);
            } else if self.take(b'/') {
                let right = self.unary(depth);
                left = self.node(Operation::Divide, left, right, 0.0);
            } else {
                break;
            }
        }
        left
    }

    fn unary(&mut self, depth: usize) -> Option<usize> {
        for (byte, operation) in [(b'+', Operation::Positive), (b'-', Operation::Negative)] {
            if self.take(byte) {
                if !self.descend(depth) {
                    return None;
                }
                let operand = self.unary(depth + 1);
                return self.node(operation, operand, None, 0.0);
            }
        }
        let base = self.atom(depth);
        if self.ok() && self.take(b'^') {
            if !self.descend(depth) {
                return None;
            }
            let exponent = self.unary(depth + 1);
            return self.node(Operation::Power, base, exponent, 0.0);
        }
        base
    }

    fn atom(&mut self, depth: usize) -> Option<usize> {
        self.skip();
        if self.take(b'(') {
            if !self.descend(depth) {
                return None;
            }
            let inner = self.sum(depth + 1);
            if !self.take(b')') {
                self.fail(CompileLimit::None);
            }
            return inner;
        }
        let Some(&first) = self.source.get(self.position) else {
            self.fail(CompileLimit::None);
            return None;
        };
        let start = self.position;
        if first.is_ascii_digit() || first == b'.' {
            let mut digits = false;
            while self
                .source
                .get(self.position)
                .is_some_and(u8::is_ascii_digit)
            {
                self.position += 1;
                digits = true;
            }
            if self.source.get(self.position) == Some(&b'.') {
                self.position += 1;
                while self
                    .source
                    .get(self.position)
                    .is_some_and(u8::is_ascii_digit)
                {
                    self.position += 1;
                    digits = true;
                }
            }
            if !digits {
                self.fail(CompileLimit::None);
                return None;
            }
            if matches!(self.source.get(self.position), Some(b'e' | b'E')) {
                self.position += 1;
                if matches!(self.source.get(self.position), Some(b'+' | b'-')) {
                    self.position += 1;
                }
                let exponent = self.position;
                while self
                    .source
                    .get(self.position)
                    .is_some_and(u8::is_ascii_digit)
                {
                    self.position += 1;
                }
                if self.position == exponent {
                    self.fail(CompileLimit::None);
                    return None;
                }
            }
            let Ok(value) = self.text[start..self.position].parse::<f64>() else {
                self.fail(CompileLimit::None);
                return None;
            };
            if !value.is_finite() {
                self.fail(CompileLimit::None);
                return None;
            }
            return self.node(Operation::Literal, None, None, value);
        }
        if !(first.is_ascii_alphabetic() || first == b'_') {
            self.fail(CompileLimit::None);
            return None;
        }
        while self
            .source
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.position += 1;
        }
        let name = &self.text[start..self.position];
        let constant = match name {
            "x" => Some(Operation::X),
            "pi" => Some(Operation::Pi),
            "e" => Some(Operation::E),
            _ => None,
        };
        if let Some(operation) = constant {
            return self.node(operation, None, None, 0.0);
        }
        let operation = match name {
            "sin" => Operation::Sin,
            "cos" => Operation::Cos,
            "tan" => Operation::Tan,
            "exp" => Operation::Exp,
            "log" => Operation::Log,
            "sqrt" => Operation::Sqrt,
            "abs" => Operation::Abs,
            _ => {
                self.status = CompileStatus::UnsupportedExpression;
                return None;
            }
        };
        if !self.take(b'(') {
            self.fail(CompileLimit::None);
            return None;
        }
        if !self.descend(depth) {
            return None;
        }
        let argument = self.sum(depth + 1);
        if !self.take(b')') {
            self.fail(CompileLimit::None);
        }
        self.node(operation, argument, None, 0.0)
    }
}

pub fn compile(source: &str) -> Compilation {
    let mut parser = Parser {
        source: source.as_bytes(),
        text: source,
        position: 0,
        budget: MAXIMUM_NODES,
        status: CompileStatus::Success,
        limit: CompileLimit::None,
        nodes: Vec::new(),
    };
    let mut root = None;
    if source.len() > MAXIMUM_SOURCE_BYTES {
        parser.fail(CompileLimit::SourceBytes);
    } else {
        root = parser.sum(0);
        parser.skip();
        if parser.position != source.len() {
            parser.fail(CompileLimit::None);
        }
    }
    let expression = match (parser.status, root) {
        (CompileStatus::Success, Some(root)) => Expression {
            nodes: parser.nodes,
            root,
        },
        _ => Expression::default(),
    };
    let status = if parser.status == CompileStatus::Success && expression.nodes.is_empty() {
        CompileStatus::InvalidExpression
    } else {
        parser.status
    };
    Compilation {
        status,
        limit: parser.limit,
        offset: parser.position,
        expression,
    }
}

impl Expression {
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn evaluate(&self, x: f64) -> Evaluation {
        use EvaluationStatus as S;
        if !x.is_finite() || self.nodes.is_empty() {
            return Evaluation {
                status: S::UndefinedDomain,
                value: 0.0,
            };
        }
        let finite = |value| Evaluation {
            status: S::Finite,
            value,
        };
        let mut values: Vec<Evaluation> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let operand = |index: Option<usize>| {
                index
                    .and_then(|index| values.get(index).copied())
                    .unwrap_or(finite(0.0))
            };
            let a = operand(node.left);
            let b = operand(node.right);
            if a.status != S::Finite || b.status != S::Finite {
                values.push(if a.status != S::Finite { a } else { b });
                continue;
            }
            let mut status = S::Finite;
            let value = match node.operation {
                Operation::Literal => node.literal,
                Operation::X => x,
                Operation::Pi => std::f64::consts::PI,
                Operation::E => std::f64::consts::E,
                Operation::Positive => a.value,
                Operation::Negative => -a.value,
                Operation::Add => a.value + b.value,
                Operation::Subtract => a.value - b.value,
                Operation::Multiply => a.value * b.value,
                Operation::Divide => {
                    if b.value == 0.0 {
                        status = S::UndefinedDivision;
                        0.0
                    } else {
                        a.value / b.value
                    }
                }
                Operation::Power => {
                    let value = a.value.powf(b.value);
                    if value.is_nan() {
                        status = S::UndefinedDomain;
                    }
                    value
                }
                Operation::Sin => a.value.sin(),
                Operation::Cos => a.value.cos(),
                Operation::Tan => {
                    if a.value.cos().abs() < 1e-12 {
                        status = S::UndefinedDomain;
                        0.0
                    } else {
                        a.value.tan()
                    }
                }
                Operation::Exp => a.value.exp(),
                Operation::Log => {
                    if a.value <= 0.0 {
                        status = S::UndefinedDomain;
                        0.0
                    } else {
                        a.value.ln()
                    }
                }
                Operation::Sqrt => {
                    if a.value < 0.0 {
                        status = S::UndefinedDomain;
                        0.0
                    } else {
                        a.value.sqrt()
                    }
                }
                Operation::Abs => a.value.abs(),
            };
            if status == S::Finite && !value.is_finite() {
                status = S::UndefinedOverflow;
            }
            values.push(Evaluation {
                status,
                value: if status == S::Finite { value } else { 0.0 },
            });
        }
        values.get(self.root).copied().unwrap_or(Evaluation {
            status: S::UndefinedDomain,
            value: 0.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(source: &str, x: f64) -> f64 {
        let compiled = compile(source);
        assert_eq!(compiled.status, CompileStatus::Success, "{source}");
        let evaluation = compiled.expression.evaluate(x);
        assert_eq!(evaluation.status, EvaluationStatus::Finite, "{source}");
        evaluation.value
    }

    #[test]
    fn precedence_and_functions() {
        assert_eq!(value("1 + 2 * 3", 0.0), 7.0);
        assert_eq!(value("2^3^2", 0.0), 512.0, "冪は右結合");
        assert_eq!(value("-x^2", 3.0), -9.0, "単項の符号は冪より弱い");
        assert_eq!(value("(1 + 2) * 3", 0.0), 9.0);
        assert_eq!(value("x / 4", 2.0), 0.5);
        assert!((value("sin(pi / 2)", 0.0) - 1.0).abs() < 1e-12);
        assert!((value("log(e)", 0.0) - 1.0).abs() < 1e-12);
        assert_eq!(value("sqrt(abs(-16))", 0.0), 4.0);
        assert_eq!(value("1.5e2 + .5", 0.0), 150.5);
    }

    #[test]
    fn invalid_and_unsupported_sources() {
        assert_eq!(compile("").status, CompileStatus::InvalidExpression);
        assert_eq!(compile("1 +").status, CompileStatus::InvalidExpression);
        assert_eq!(compile("(1").status, CompileStatus::InvalidExpression);
        assert_eq!(compile("1e").status, CompileStatus::InvalidExpression);
        assert_eq!(compile("x y").status, CompileStatus::InvalidExpression);
        assert_eq!(
            compile("floor(x)").status,
            CompileStatus::UnsupportedExpression
        );
        assert_eq!(compile("sin x").status, CompileStatus::InvalidExpression);
        let deep = format!("{}x{}", "(".repeat(80), ")".repeat(80));
        let compiled = compile(&deep);
        assert_eq!(compiled.limit, CompileLimit::Recursion);
        assert_eq!(compile(&"1+".repeat(3000)).limit, CompileLimit::SourceBytes);
    }

    #[test]
    fn undefined_points_are_reported_not_hidden() {
        let check = |source: &str, x: f64, want: EvaluationStatus| {
            assert_eq!(
                compile(source).expression.evaluate(x).status,
                want,
                "{source} at {x}"
            );
        };
        check("1 / x", 0.0, EvaluationStatus::UndefinedDivision);
        check("log(x)", 0.0, EvaluationStatus::UndefinedDomain);
        check("sqrt(x)", -1.0, EvaluationStatus::UndefinedDomain);
        check("(-8)^(1/3)", 0.0, EvaluationStatus::UndefinedDomain);
        check("exp(x)", 1000.0, EvaluationStatus::UndefinedOverflow);
        check(
            "tan(x)",
            std::f64::consts::FRAC_PI_2,
            EvaluationStatus::UndefinedDomain,
        );
    }
}
