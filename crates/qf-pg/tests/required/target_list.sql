-- expect: email=nullable
SELECT email = :email AS same FROM customers
