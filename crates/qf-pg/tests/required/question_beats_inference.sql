-- expect: email=nullable:annotation
SELECT id FROM customers WHERE email = :email AND id > 0 AND email = :email?
