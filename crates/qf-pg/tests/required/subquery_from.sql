-- expect: email=nullable
SELECT s.id FROM (SELECT id FROM customers WHERE email = :email) s
