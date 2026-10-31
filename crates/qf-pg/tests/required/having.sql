-- expect: email=nullable
SELECT email FROM customers GROUP BY email HAVING email = :email
