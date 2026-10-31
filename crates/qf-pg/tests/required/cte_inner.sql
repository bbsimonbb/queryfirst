-- expect: email=nullable
WITH x AS (SELECT id FROM customers WHERE email = :email) SELECT id FROM x
