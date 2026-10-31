-- expect: email=nullable
SELECT id FROM customers WHERE email OPERATOR(pg_catalog.=) :email
