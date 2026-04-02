"""Example python module 8 for indexing tests."""

def mutate_obj_8(obj):
    obj = obj + 8
    return obj

class Worker8:
    def apply(self, value):
        return mutate_obj_8(value)
