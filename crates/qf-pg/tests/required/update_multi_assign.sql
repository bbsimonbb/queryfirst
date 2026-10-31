-- expect: email=nullable name=nullable
UPDATE customers SET (email, name) = (:email, :name)
