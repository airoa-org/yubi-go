//! Solid's `createSignal` across threads: the setter sends, the getter keeps
//! whatever arrived last. Every value travels by `mpsc`, so neither side ever
//! waits on the other's lock.

use std::sync::mpsc::{channel, Receiver, Sender};

pub struct Getter<T> {
    rx: Receiver<T>,
    value: T,
}

pub struct Setter<T>(Sender<T>);

pub fn signal<T>(initial: T) -> (Getter<T>, Setter<T>) {
    let (tx, rx) = channel();
    (Getter { rx, value: initial }, Setter(tx))
}

impl<T> Getter<T> {
    pub fn get(&mut self) -> &T {
        if let Some(v) = self.rx.try_iter().last() {
            self.value = v;
        }
        &self.value
    }
}

impl<T> Setter<T> {
    /// After the getter is dropped, the value goes nowhere.
    pub fn set(&self, value: T) {
        let _ = self.0.send(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_getter_reads_the_last_value_set() {
        let (mut get, set) = signal(0);
        assert_eq!(*get.get(), 0);
        std::thread::spawn(move || (1..=3).for_each(|v| set.set(v))).join().unwrap();
        assert_eq!(*get.get(), 3);
    }
}
