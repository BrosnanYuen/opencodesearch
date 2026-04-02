"""Example python module 7 for indexing tests."""

def mutate_obj_7(obj):
    obj = obj + 7
    return obj

class Worker7:
    def apply(self, value):
        return mutate_obj_7(value)
