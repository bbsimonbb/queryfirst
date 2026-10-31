-- expect: email=required:where-eq-notnull
DELETE FROM customers WHERE email = :email RETURNING id
