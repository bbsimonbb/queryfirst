-- expect: email=nullable
SELECT id FROM customers WHERE email = :email UNION SELECT id FROM addresses
