//! Signatures of the builtin methods, by receiver type. They mirror what
//! `lacon_interp::builtins` implements.

use lacon_interp::builtins::is_str_method;

use crate::infer::Checker;
use crate::types::{T, INT};

/// What a builtin method takes and returns, for a given receiver.
pub struct Sig {
    pub params: Vec<T>,
    pub required: usize,
    /// The last parameter repeats (`xs.push(a, b)`).
    pub variadic: bool,
    pub ret: T,
    /// Argument `i` is any iterable whose elements flow into the type.
    pub iter_args: Vec<(usize, T)>,
    /// `min`, `max`, `clamp` and `pow` on an int: the result is an int when
    /// every argument is, otherwise a float.
    pub num_mix: bool,
    /// The result is a list of the elements of the first type, collected
    /// into the second (`flat_map`, `flatten`).
    pub elems_of: Option<(T, T)>,
}

impl Sig {
    fn new(params: Vec<T>, ret: T) -> Sig {
        let required = params.len();
        Sig { params, required, variadic: false, ret, iter_args: vec![], num_mix: false, elems_of: None }
    }

    fn opt(params: Vec<T>, required: usize, ret: T) -> Sig {
        Sig { required, ..Sig::new(params, ret) }
    }

    fn iter(mut self, i: usize, elem: T) -> Sig {
        if self.params.len() <= i {
            self.params.resize(i + 1, T::Unknown);
            self.required = self.required.max(i + 1);
        }
        self.params[i] = T::Unknown;
        self.iter_args.push((i, elem));
        self
    }
}

pub enum NoSig {
    /// No such method on this receiver; the message.
    Missing(String),
    /// A mutator called on a string.
    Immutable,
}

const FLOAT_FNS: &[&str] = &["sqrt", "cbrt", "exp", "ln", "log2", "log10", "sin", "cos", "tan", "asin", "acos", "atan", "fract"];

impl Checker<'_> {
    /// The elements a value iterates over in methods (map entries are
    /// `(k, v)` pairs). `None` for types that aren't iterable.
    pub fn elem_of(&mut self, t: &T) -> Option<T> {
        Some(match self.s.resolve(t) {
            T::List(e) | T::Set(e) | T::Heap(e) => *e,
            T::Map(k, v) => T::Tuple(vec![*k, *v]),
            T::Range => INT,
            T::Str => T::Str,
            T::Unknown | T::Param(_) | T::Never => T::Unknown,
            T::Tuple(ts) => {
                let mut acc = T::Never;
                for t in &ts {
                    acc = self.s.join(&acc, t);
                }
                acc
            }
            _ => return None,
        })
    }

    /// The signature of builtin method `name` on a receiver of type `recv`
    /// (already resolved, not an optional, result or variable). `fn_args`
    /// says which arguments are lambdas or function names.
    pub fn method_sig(&mut self, recv: &T, name: &str, nargs: usize, fn_args: &[bool]) -> Result<Sig, NoSig> {
        let recv = self.s.resolve(recv);
        let first_is_fn = fn_args.first().copied().unwrap_or(false);
        // Methods every value has.
        match name {
            "str" | "to_string" => return Ok(Sig::new(vec![], T::Str)),
            "clone" | "iter" | "into_iter" | "to_owned" | "copied" | "cloned" | "as_str" => return Ok(Sig::new(vec![], recv)),
            "collect" => {
                let ret = match &recv {
                    T::Range | T::Set(_) | T::Heap(_) => T::list(self.elem_of(&recv).unwrap_or(T::Unknown)),
                    _ => recv.clone(),
                };
                return Ok(Sig::new(vec![], ret));
            }
            "unwrap" => return Ok(Sig::new(vec![], recv)),
            "expect" => return Ok(Sig::opt(vec![T::Str], 0, recv)),
            "is_none" | "is_some" | "is_err" | "is_ok" => return Ok(Sig::new(vec![], T::Bool)),
            _ => {}
        }
        let missing = |c: &Checker, hint: &str| NoSig::Missing(format!("{} has no method `{name}`{hint}", c.show(&recv)));
        match &recv {
            T::Param(_) => {
                let params = vec![T::Unknown; nargs];
                Ok(Sig::new(params, T::Unknown))
            }
            T::Int(_) => self.int_method(name, nargs).ok_or_else(|| missing(self, "")),
            T::Float => self.float_method(name, nargs).ok_or_else(|| missing(self, "")),
            T::Str => {
                if lacon_interp::builtins::is_mutator(name) {
                    return Err(NoSig::Immutable);
                }
                if let Some(s) = self.str_method(name, nargs, first_is_fn) {
                    return Ok(s);
                }
                self.iter_method(T::Str, name, nargs, first_is_fn).ok_or_else(|| missing(self, ""))
            }
            T::List(e) => {
                let e = (**e).clone();
                let s = match name {
                    "push" => Some(Sig { variadic: true, ..Sig::new(vec![e.clone()], T::Unit) }),
                    "pop" if nargs == 0 => Some(Sig::new(vec![], T::opt(e.clone()))),
                    "pop" => Some(Sig::new(vec![INT], e.clone())),
                    "insert" => Some(Sig::new(vec![INT, e.clone()], T::Unit)),
                    "remove" => Some(Sig::new(vec![INT], e.clone())),
                    "clear" => Some(Sig::new(vec![], T::Unit)),
                    "extend" => Some(Sig::new(vec![], T::Unit).iter(0, e.clone())),
                    "swap" => Some(Sig::new(vec![INT, INT], T::Unit)),
                    "truncate" => Some(Sig::new(vec![INT], T::Unit)),
                    "retain" => Some(Sig::new(vec![T::func(vec![e.clone()], T::Bool)], T::Unit)),
                    "repeat" => Some(Sig::new(vec![INT], T::list(e.clone()))),
                    "add" => return Err(missing(self, ": lists use `push`")),
                    _ => None,
                };
                match s {
                    Some(s) => Ok(s),
                    None => {
                        let hint = if matches!(self.s.resolve(&e), T::Str) && is_str_method(name) {
                            format!("; to apply it to each item: `.map(it.{name}())`")
                        } else {
                            String::new()
                        };
                        self.iter_method(e, name, nargs, first_is_fn).ok_or_else(|| missing(self, &hint))
                    }
                }
            }
            T::Map(k, v) => {
                let (k, v) = ((**k).clone(), (**v).clone());
                let s = match name {
                    "keys" => Some(Sig::new(vec![], T::list(k.clone()))),
                    "values" => Some(Sig::new(vec![], T::list(v.clone()))),
                    "items" | "to_list" => Some(Sig::new(vec![], T::list(T::Tuple(vec![k.clone(), v.clone()])))),
                    "get" if nargs >= 2 => Some(Sig::new(vec![k.clone(), v.clone()], v.clone())),
                    "get" => Some(Sig::new(vec![k.clone()], T::opt(v.clone()))),
                    "contains" | "contains_key" => Some(Sig::new(vec![k.clone()], T::Bool)),
                    "to_map" => Some(Sig::new(vec![], recv.clone())),
                    "insert" => Some(Sig::new(vec![k.clone(), v.clone()], T::opt(v.clone()))),
                    "remove" | "pop" => Some(Sig::new(vec![k.clone()], T::opt(v.clone()))),
                    "clear" => Some(Sig::new(vec![], T::Unit)),
                    "extend" => Some(Sig::new(vec![], T::Unit).iter(0, T::Tuple(vec![k.clone(), v.clone()]))),
                    "push" | "add" => return Err(missing(self, "; set entries with `m[k] = v`")),
                    _ => None,
                };
                match s {
                    Some(s) => Ok(s),
                    None => self.iter_method(T::Tuple(vec![k, v]), name, nargs, first_is_fn).ok_or_else(|| missing(self, "")),
                }
            }
            T::Set(e) => {
                let e = (**e).clone();
                let s = match name {
                    "contains" => Some(Sig::new(vec![e.clone()], T::Bool)),
                    "to_set" => Some(Sig::new(vec![], recv.clone())),
                    "union" | "intersection" | "difference" => Some(Sig::new(vec![], recv.clone()).iter(0, e.clone())),
                    "is_subset" => Some(Sig::new(vec![], T::Bool).iter(0, e.clone())),
                    "add" | "insert" => Some(Sig::new(vec![e.clone()], T::Bool)),
                    "remove" => Some(Sig::new(vec![e.clone()], T::Bool)),
                    "clear" => Some(Sig::new(vec![], T::Unit)),
                    "extend" => Some(Sig::new(vec![], T::Unit).iter(0, e.clone())),
                    "push" => return Err(missing(self, ": sets use `add`")),
                    _ => None,
                };
                match s {
                    Some(s) => Ok(s),
                    None => self.iter_method(e, name, nargs, first_is_fn).ok_or_else(|| missing(self, "")),
                }
            }
            T::Heap(e) => {
                let e = (**e).clone();
                let s = match name {
                    "first" | "peek" | "min" if nargs == 0 => Some(Sig::new(vec![], T::opt(e.clone()))),
                    "push" | "add" => Some(Sig { variadic: true, ..Sig::new(vec![e.clone()], T::Unit) }),
                    "pop" => Some(Sig::new(vec![], T::opt(e.clone()))),
                    "clear" => Some(Sig::new(vec![], T::Unit)),
                    "extend" => Some(Sig::new(vec![], T::Unit).iter(0, e.clone())),
                    _ => None,
                };
                match s {
                    Some(s) => Ok(s),
                    None => self.iter_method(e, name, nargs, first_is_fn).ok_or_else(|| missing(self, "")),
                }
            }
            T::Range => {
                let s = match name {
                    "rev" => Some(Sig::new(vec![], T::Range)),
                    "step_by" => Some(Sig::new(vec![INT], T::Range)),
                    "sum" => Some(Sig::new(vec![], INT)),
                    _ => None,
                };
                match s {
                    Some(s) => Ok(s),
                    None => self.iter_method(INT, name, nargs, first_is_fn).ok_or_else(|| missing(self, "")),
                }
            }
            T::Tuple(_) => {
                if name == "len" {
                    return Ok(Sig::new(vec![], INT));
                }
                let e = self.elem_of(&recv).unwrap_or(T::Unknown);
                self.iter_method(e, name, nargs, first_is_fn).ok_or_else(|| missing(self, ""))
            }
            _ => Err(missing(self, "")),
        }
    }

    fn int_method(&mut self, name: &str, nargs: usize) -> Option<Sig> {
        Some(match name {
            "abs" | "sign" => Sig::new(vec![], INT),
            "chr" => Sig::new(vec![], T::Str),
            "floor" | "ceil" | "trunc" => Sig::new(vec![], INT),
            "round" if nargs == 0 => Sig::new(vec![], INT),
            "round" => Sig::new(vec![INT], T::Float),
            "pow" | "min" | "max" => {
                let n = self.s.fresh();
                Sig { num_mix: true, ..Sig::new(vec![n], INT) }
            }
            "clamp" => {
                let (a, b) = (self.s.fresh(), self.s.fresh());
                Sig { num_mix: true, ..Sig::new(vec![a, b], INT) }
            }
            "log" => Sig::opt(vec![T::Float], 0, T::Float),
            "atan2" | "hypot" => Sig::new(vec![T::Float], T::Float),
            "is_nan" | "is_finite" => Sig::new(vec![], T::Bool),
            n if FLOAT_FNS.contains(&n) => Sig::new(vec![], T::Float),
            _ => return None,
        })
    }

    fn float_method(&mut self, name: &str, _nargs: usize) -> Option<Sig> {
        Some(match name {
            "abs" | "sign" | "floor" | "ceil" | "trunc" => Sig::new(vec![], T::Float),
            "round" => Sig::opt(vec![INT], 0, T::Float),
            "pow" | "min" | "max" | "atan2" | "hypot" => Sig::new(vec![T::Float], T::Float),
            "log" => Sig::opt(vec![T::Float], 0, T::Float),
            "clamp" => Sig::new(vec![T::Float, T::Float], T::Float),
            "is_nan" | "is_finite" => Sig::new(vec![], T::Bool),
            n if FLOAT_FNS.contains(&n) => Sig::new(vec![], T::Float),
            _ => return None,
        })
    }

    fn str_method(&mut self, name: &str, nargs: usize, first_is_fn: bool) -> Option<Sig> {
        let strs = || T::list(T::Str);
        Some(match name {
            "len" => Sig::new(vec![], INT),
            "is_empty" => Sig::new(vec![], T::Bool),
            "chars" | "lines" | "words" => Sig::new(vec![], strs()),
            "bytes" => Sig::new(vec![], T::list(INT)),
            "split" => Sig::opt(vec![T::Str], 0, strs()),
            "split_once" => Sig::new(vec![T::Str], T::opt(T::Tuple(vec![T::Str, T::Str]))),
            "trim" | "trim_start" | "trim_end" => Sig::opt(vec![T::Str], 0, T::Str),
            "upper" | "lower" | "capitalize" | "rev" | "sort" => Sig::new(vec![], T::Str),
            "starts_with" | "ends_with" | "contains" => Sig::new(vec![T::Str], T::Bool),
            "find" | "index" if !first_is_fn => Sig::new(vec![T::Str], T::opt(INT)),
            "rfind" => Sig::new(vec![T::Str], T::opt(INT)),
            "replace" => Sig::new(vec![T::Str, T::Str], T::Str),
            "repeat" => Sig::new(vec![INT], T::Str),
            "parse" => {
                let v = self.s.fresh();
                Sig::new(vec![], T::res(v))
            }
            "count" if nargs == 1 && !first_is_fn => Sig::new(vec![T::Str], INT),
            "is_digit" | "is_alpha" | "is_alnum" | "is_space" | "is_upper" | "is_lower" => Sig::new(vec![], T::Bool),
            "ord" => Sig::new(vec![], INT),
            "join" => Sig::new(vec![], T::Str).iter(0, T::Unknown),
            "strip_prefix" | "strip_suffix" => Sig::new(vec![T::Str], T::opt(T::Str)),
            "pad_left" | "pad_right" => Sig::opt(vec![INT, T::Str], 1, T::Str),
            "get" => Sig::new(vec![INT], T::opt(T::Str)),
            "first" | "last" => Sig::new(vec![], T::opt(T::Str)),
            _ => return None,
        })
    }

    /// Methods every iterable has, over elements of type `e`.
    fn iter_method(&mut self, e: T, name: &str, nargs: usize, first_is_fn: bool) -> Option<Sig> {
        let pred = || T::func(vec![e.clone()], T::Bool);
        let list = || T::list(e.clone());
        Some(match name {
            "len" => Sig::new(vec![], INT),
            "is_empty" => Sig::new(vec![], T::Bool),
            "first" | "last" => Sig::new(vec![], T::opt(e.clone())),
            "get" => Sig::new(vec![INT], T::opt(e.clone())),
            "contains" => Sig::new(vec![e.clone()], T::Bool),
            "index" => Sig::new(vec![e.clone()], T::opt(INT)),
            "to_list" => Sig::new(vec![], list()),
            "to_set" => Sig::new(vec![], T::set(e.clone())),
            "to_map" => {
                let (k, v) = (self.s.fresh(), self.s.fresh());
                if !self.s.unify(&e, &T::Tuple(vec![k.clone(), v.clone()])) {
                    return None;
                }
                Sig::new(vec![], T::map(k, v))
            }
            "map" => {
                let u = self.s.fresh();
                Sig::new(vec![T::func(vec![e.clone()], u.clone())], T::list(u))
            }
            "filter" | "take_while" | "skip_while" => Sig::new(vec![pred()], list()),
            "find" => Sig::new(vec![pred()], T::opt(e.clone())),
            "position" => Sig::new(vec![pred()], T::opt(INT)),
            "any" | "all" => Sig::opt(vec![pred()], 0, T::Bool),
            "count" if nargs == 0 => Sig::new(vec![], INT),
            "count" if first_is_fn => Sig::new(vec![pred()], INT),
            "count" => Sig::new(vec![e.clone()], INT),
            "sum" | "product" | "min" | "max" if nargs == 0 => Sig::new(vec![], e.clone()),
            "min_by" | "max_by" => {
                let k = self.s.fresh();
                Sig::new(vec![T::func(vec![e.clone()], k)], e.clone())
            }
            "sort" | "rev" | "unique" => Sig::new(vec![], list()),
            "sort_by" => {
                let k = self.s.fresh();
                Sig::new(vec![T::func(vec![e.clone()], k)], list())
            }
            "enumerate" => Sig::opt(vec![INT], 0, T::list(T::Tuple(vec![INT, e.clone()]))),
            "zip" => {
                let u = self.s.fresh();
                Sig::new(vec![], T::list(T::Tuple(vec![e.clone(), u.clone()]))).iter(0, u)
            }
            "flat_map" => {
                let (r, x) = (self.s.fresh(), self.s.fresh());
                Sig { elems_of: Some((r.clone(), x.clone())), ..Sig::new(vec![T::func(vec![e.clone()], r)], T::list(x)) }
            }
            "flatten" => {
                let x = self.s.fresh();
                Sig { elems_of: Some((e.clone(), x.clone())), ..Sig::new(vec![], T::list(x)) }
            }
            "take" | "skip" | "step_by" => Sig::new(vec![INT], list()),
            "chunks" | "windows" => Sig::new(vec![INT], T::list(list())),
            "join" => Sig::opt(vec![T::Str], 0, T::Str),
            "group_by" => {
                let k = self.s.fresh();
                Sig::new(vec![T::func(vec![e.clone()], k.clone())], T::map(k, list()))
            }
            "partition" => Sig::new(vec![pred()], T::Tuple(vec![list(), list()])),
            "fold" => {
                let a = self.s.fresh();
                Sig::new(vec![a.clone(), T::func(vec![a.clone(), e.clone()], a.clone())], a)
            }
            "reduce" => Sig::new(vec![T::func(vec![e.clone(), e.clone()], e.clone())], e.clone()),
            "each" => {
                let u = self.s.fresh();
                Sig::new(vec![T::func(vec![e.clone()], u)], T::Unit)
            }
            _ => return None,
        })
    }
}
