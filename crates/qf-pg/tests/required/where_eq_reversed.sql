-- expect: email=required:where-eq-notnull id=required:where-eq-notnull
SELECT id FROM customers WHERE :email = email AND (customers.id = :id AND true)
