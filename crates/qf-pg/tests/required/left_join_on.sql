-- expect: email=nullable line1=nullable
SELECT c.id FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id AND c.email = :email AND a.line1 = :line1
