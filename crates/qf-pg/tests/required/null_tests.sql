-- expect: email=nullable
SELECT id FROM customers WHERE :email::text IS NOT NULL AND email = :email
