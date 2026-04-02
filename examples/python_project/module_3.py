"""Example python module 3 for indexing tests."""

def mutate_obj_3(obj):
    obj = obj + 3
    return obj

class Worker3:
    def apply(self, value):
        return mutate_obj_3(value)
