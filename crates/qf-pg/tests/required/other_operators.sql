-- expect: a=nullable b=nullable c=nullable
SELECT id FROM customers WHERE id > :a AND email <> :b AND email LIKE :c
