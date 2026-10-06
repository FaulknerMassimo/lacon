use std::cell::RefCell;
use std::rc::Rc;

use crate::value::Value;

/// Local variable slots for one function or lambda invocation. Lambdas keep
/// their defining frame alive through `parent`.
pub struct Frame {
    pub slots: RefCell<Vec<Value>>,
    pub parent: Option<Rc<Frame>>,
}

impl Frame {
    pub fn new(n: u32, parent: Option<Rc<Frame>>) -> Rc<Frame> {
        Rc::new(Frame { slots: RefCell::new(vec![Value::Unit; n as usize]), parent })
    }

    pub fn up(self: &Rc<Frame>, depth: u32) -> &Rc<Frame> {
        let mut f = self;
        for _ in 0..depth {
            f = f.parent.as_ref().expect("closure frame");
        }
        f
    }

    pub fn get(&self, slot: u32) -> Value {
        self.slots.borrow()[slot as usize].clone()
    }

    pub fn set(&self, slot: u32, v: Value) {
        self.slots.borrow_mut()[slot as usize] = v;
    }
}
