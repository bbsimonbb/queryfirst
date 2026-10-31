-- expect: email=nullable
SELECT id FROM customers WHERE :email IS NOT NULL AND email = :email
