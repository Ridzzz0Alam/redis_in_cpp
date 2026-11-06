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
