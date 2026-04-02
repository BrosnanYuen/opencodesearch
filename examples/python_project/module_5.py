"""Example python module 5 for indexing tests."""

def mutate_obj_5(obj):
    obj = obj + 5
    return obj

class Worker5:
    def apply(self, value):
        return mutate_obj_5(value)
