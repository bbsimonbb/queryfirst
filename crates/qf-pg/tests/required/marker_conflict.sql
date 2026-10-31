-- expect: email=nullable:annotation
SELECT id FROM customers WHERE email = :email! AND name = :email?
