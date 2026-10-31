-- expect: email=nullable
SELECT id FROM customers WHERE NOT (email = :email)
