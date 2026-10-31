-- expect: email=nullable
SELECT id FROM customers WHERE email = :email OR name = 'x'
