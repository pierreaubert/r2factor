//! A made-up "evaluator + parser + token" file with mixed concerns.

use std::collections::HashMap;

pub const MAX_DEPTH: usize = 64;
pub const DEFAULT_NAME: &str = "anon";
pub const VERSION: u32 = 3;

macro_rules! tok {
    ($k:ident) => { Token::$k };
}

#[derive(Debug, Clone)]
pub enum Token {
    LParen,
    RParen,
    Ident(String),
    Number(i64),
}

#[derive(Debug)]
pub struct Lexer<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }
    pub fn next_token(&mut self) -> Option<Token> {
        let _ = self.src.len();
        self.pos += 1;
        Some(Token::LParen)
    }
}

#[derive(Debug)]
pub struct Parser {
    tokens: Vec<Token>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens }
    }
    pub fn parse_expr(&mut self) -> Expr {
        Expr::Nil
    }
    pub fn parse_program(&mut self) -> Vec<Expr> {
        vec![self.parse_expr()]
    }
}

#[derive(Debug, Clone)]
pub enum Expr {
    Nil,
    Num(i64),
    Sym(String),
    Call(Box<Expr>, Vec<Expr>),
}

pub struct Env {
    map: HashMap<String, Expr>,
}

impl Env {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }
    pub fn lookup(&self, name: &str) -> Option<&Expr> {
        self.map.get(name)
    }
}

pub fn eval(expr: &Expr, env: &Env) -> Result<Expr, EvalError> {
    match expr {
        Expr::Nil => Ok(Expr::Nil),
        Expr::Num(n) => Ok(Expr::Num(*n)),
        Expr::Sym(s) => env.lookup(s).cloned().ok_or_else(|| EvalError::Unbound(s.clone())),
        Expr::Call(f, args) => eval_call(f, args, env),
    }
}

pub fn eval_call(f: &Expr, args: &[Expr], env: &Env) -> Result<Expr, EvalError> {
    let _ = (f, args, env);
    Ok(Expr::Nil)
}

pub fn eval_program(prog: &[Expr], env: &Env) -> Result<Expr, EvalError> {
    let mut last = Expr::Nil;
    for e in prog {
        last = eval(e, env)?;
    }
    Ok(last)
}

#[derive(Debug)]
pub enum EvalError {
    Unbound(String),
    BadCall,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for EvalError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke() {
        let env = Env::new();
        assert!(eval(&Expr::Nil, &env).is_ok());
    }
}
