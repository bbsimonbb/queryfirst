-- expect: email=nullable
SELECT id FROM customers WHERE email = :email AND name = COALESCE(:email, name)
