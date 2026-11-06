//! Port of `tests/test_list.cpp`.

use redis_rust::list::DList;

#[test]
fn fifo_order_and_detach() {
    let mut list = DList::new();
    assert!(list.is_empty());

    let a = list.push_back(1);
    let _b = list.push_back(2);
    let _c = list.push_back(3);
    assert!(!list.is_empty());
    assert_eq!(list.front(), Some(1));
    assert_eq!(list.values(), vec![1, 2, 3]);

    // Move `a` to the back (what the server does on activity).
    list.move_to_back(a);
    assert_eq!(list.values(), vec![2, 3, 1]);

    list.detach(a);
    list.detach(_b);
    list.detach(_c);
    assert!(list.is_empty());
}

#[test]
fn front_id_and_reuse() {
    let mut list = DList::new();
    assert_eq!(list.front_id(), None);
    let a = list.push_back(10);
    let b = list.push_back(20);
    assert_eq!(list.front_id(), Some(a));
    list.detach(a);
    assert_eq!(list.front_id(), Some(b));
    let c = list.push_back(30); // reuses a's slot
    assert_eq!(list.values(), vec![20, 30]);
    assert_eq!(list.len(), 2);
    list.detach(b);
    list.detach(c);
    assert!(list.is_empty());
}
