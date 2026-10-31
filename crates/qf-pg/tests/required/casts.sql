-- expect: email=required:where-eq-notnull id=required:where-eq-notnull
SELECT id FROM customers WHERE email = :email::text AND id = CAST(:id AS int)
