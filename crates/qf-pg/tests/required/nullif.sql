-- expect: email=nullable
SELECT id FROM customers WHERE email = NULLIF(:email, '')
