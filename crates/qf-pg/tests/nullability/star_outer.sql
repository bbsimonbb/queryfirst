-- expect: email=notnull id=nullable:outer-join customer_id=nullable:outer-join line1=nullable:outer-join city=nullable:outer-join postcode=nullable:outer-join
SELECT c.email, a.* FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id
