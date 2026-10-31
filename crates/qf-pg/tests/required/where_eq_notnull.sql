-- expect: email=required:where-eq-notnull
SELECT id FROM customers WHERE email = :email
