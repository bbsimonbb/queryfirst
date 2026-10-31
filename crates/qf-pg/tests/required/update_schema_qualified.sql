-- expect: email=required:insert-notnull
UPDATE public.customers SET email = :email
