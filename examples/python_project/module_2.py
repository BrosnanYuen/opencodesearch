"""Example python module 2 for indexing tests."""

def mutate_obj_2(obj):
    obj = obj + 2
    return obj

class Worker2:
    def apply(self, value):
        return mutate_obj_2(value)
